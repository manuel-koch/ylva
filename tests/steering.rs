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

mod common;

use std::sync::Arc;
use std::time::Duration;

use ylva::{
    AgentCommand, AgentLoop, AssistantStatus, EngineImpl, Message, MockProvider, ToolRegistry,
    UiUpdate, UserMessageKind, DEFAULT_SYSTEM_PROMPT, TOOL_PROTOCOL_SYSTEM_PROMPT,
};

use common::collect_until;

fn is_stream_started(update: &UiUpdate) -> bool {
    matches!(update, UiUpdate::StreamStarted)
}

#[tokio::test(start_paused = true)]
async fn steer_is_injected_at_natural_point_without_cancelling_stream() {
    let provider = MockProvider::new(
        vec!["first-".into(), "response".into()],
        Duration::from_millis(10),
    );
    let (mut agent, mut ui_rx) = AgentLoop::new(
        Arc::new(EngineImpl::new(
            Arc::new(provider),
            common::test_engine_config(),
        )),
        Arc::new(ToolRegistry::new()),
    );

    agent
        .send_command(AgentCommand::Prompt("first".into()))
        .await;
    let handle = agent.run();

    let before_steer = collect_until(&mut ui_rx, is_stream_started).await;
    assert_eq!(before_steer[0], UiUpdate::StreamStarted);

    // While the first stream is live, queue a steer. It must never abort.
    agent
        .send_command(AgentCommand::Prompt("steer".into()))
        .await;

    // Advance the mock interval so the first stream finishes naturally and the
    // steer is injected, starting a fresh request.
    tokio::time::advance(Duration::from_millis(10)).await;
    let around_steer = collect_until(&mut ui_rx, is_stream_started).await;

    // The first stream completed naturally: Done before the steer's own
    // StreamStarted, and no cancellation anywhere.
    let stream_dones = around_steer
        .iter()
        .filter(|update| matches!(update, UiUpdate::StreamDone))
        .count();
    assert_eq!(stream_dones, 1);
    assert!(
        !around_steer
            .iter()
            .any(|update| matches!(update, UiUpdate::StreamCancelled)),
        "steer must not abort the live stream"
    );
    assert_eq!(around_steer.last(), Some(&UiUpdate::StreamStarted));

    // SteerQueued arrives right after the steer is queued (before StreamDone);
    // SteerApplied arrives after the injection that follows StreamDone.
    let steer_queued_index = around_steer
        .iter()
        .position(|update| matches!(update, UiUpdate::SteerQueued))
        .expect("SteerQueued emitted");
    let stream_done_index = around_steer
        .iter()
        .position(|update| matches!(update, UiUpdate::StreamDone))
        .expect("StreamDone emitted");
    let steer_applied_index = around_steer
        .iter()
        .position(|update| matches!(update, UiUpdate::SteerApplied))
        .expect("SteerApplied emitted");
    assert!(
        steer_queued_index < stream_done_index && stream_done_index < steer_applied_index,
        "order: SteerQueued < StreamDone < SteerApplied"
    );

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
                content: "first".to_string(),
                kind: UserMessageKind::Explicit,
            },
            Message::Assistant {
                content: "first-response".to_string(),
                status: AssistantStatus::Complete,
                tool_calls: Vec::new(),
            },
            Message::User {
                content: "steer".to_string(),
                kind: UserMessageKind::Explicit,
            },
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn two_steers_batch_injected_before_the_next_request() {
    let provider = MockProvider::new(vec!["ok".into()], Duration::from_millis(5));
    let (mut agent, mut ui_rx) = AgentLoop::new(
        Arc::new(EngineImpl::new(
            Arc::new(provider),
            common::test_engine_config(),
        )),
        Arc::new(ToolRegistry::new()),
    );

    agent
        .send_command(AgentCommand::Prompt("first".into()))
        .await;
    let handle = agent.run();

    let _ = collect_until(&mut ui_rx, is_stream_started).await;

    // Queue two steers while streaming; both are injected before the fresh
    // request, so the context gains both user messages in order.
    agent
        .send_command(AgentCommand::Prompt("steer-a".into()))
        .await;
    agent
        .send_command(AgentCommand::Prompt("steer-b".into()))
        .await;

    tokio::time::advance(Duration::from_millis(5)).await;
    let events = collect_until(&mut ui_rx, is_stream_started).await;
    assert!(events
        .iter()
        .any(|update| matches!(update, UiUpdate::StreamDone)));

    drop(agent);
    let context = handle
        .await
        .expect("agent loop task panicked")
        .expect("agent loop returned an error");

    let user_messages: Vec<&str> = context
        .messages()
        .iter()
        .filter_map(|message| match message {
            Message::User { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(user_messages, ["first", "steer-a", "steer-b"]);
}

#[tokio::test(start_paused = true)]
async fn steer_after_stream_ended_starts_new_request() {
    let provider = MockProvider::new(vec!["done".into()], Duration::from_millis(5));
    let (mut agent, mut ui_rx) = AgentLoop::new(
        Arc::new(EngineImpl::new(
            Arc::new(provider),
            common::test_engine_config(),
        )),
        Arc::new(ToolRegistry::new()),
    );

    agent
        .send_command(AgentCommand::Prompt("first".into()))
        .await;
    let handle = agent.run();

    // Let the first request finish entirely.
    let events = collect_until(&mut ui_rx, |update| matches!(update, UiUpdate::StreamDone)).await;
    assert!(events.iter().any(is_stream_started));

    // A prompt after completion is a brand new turn, not a steer.
    agent
        .send_command(AgentCommand::Prompt("second".into()))
        .await;
    tokio::time::advance(Duration::from_millis(5)).await;
    let _ = collect_until(&mut ui_rx, is_stream_started).await;

    drop(agent);
    let context = handle
        .await
        .expect("agent loop task panicked")
        .expect("agent loop returned an error");

    let user_messages: Vec<&str> = context
        .messages()
        .iter()
        .filter_map(|message| match message {
            Message::User { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(user_messages, ["first", "second"]);
}
