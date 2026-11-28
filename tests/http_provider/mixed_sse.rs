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

//! Mixed SSE deltas must reach the agent with tool-call arguments intact.
//! The `http_provider` integration target supplies the shared fixture helpers.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;

/// Build an OpenAI-style SSE frame whose single delta carries both
/// `reasoning_content`/`reasoning` thinking text and an argument-prefix
/// `tool_calls` fragment.
fn mixed_reasoning_tool_call_frame(
    index: usize,
    function_name: &str,
    arguments_fragment: &str,
    reasoning: &str,
) -> String {
    let payload = serde_json::json!({
        "id": "chatcmpl-mock",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": "mock",
        "choices": [{
            "index": 0,
            "delta": {
                "reasoning_content": reasoning,
                "reasoning": reasoning,
                "tool_calls": [{
                    "index": index,
                    "id": "chatcmpl-tool-86eed4350a9d87cd",
                    "type": "function",
                    "function": { "name": function_name, "arguments": arguments_fragment }
                }]
            },
            "finish_reason": serde_json::Value::Null
        }]
    });
    format!("data: {payload}\n\n")
}

fn argument_frame(fragment: &str) -> String {
    let payload = serde_json::json!({"choices": [{"delta": {"tool_calls": [{
        "index": 0, "function": {"arguments": fragment}
    }]}}]});
    format!("data: {payload}\n\n")
}

/// The frame set replaying the incident: the mixed opening frame plus the
/// argument-only continuation fragments that follow it on the wire.
fn incident_argument_frames(function_name: &str) -> Vec<String> {
    vec![
        mixed_reasoning_tool_call_frame(0, function_name, "{\"command\": \"", "."),
        argument_frame("make test-coverage "),
        argument_frame("2>&1 | tail -25"),
        argument_frame("\", \"label\": "),
        argument_frame("\"Run coverage test gate\"}"),
    ]
}

/// A deterministic non-shell spy tool accepting the incident's arguments. It
/// records every invocation (assembled arguments) and auto-approves, so the
/// agent loop executes it without an approval gate.
struct IncidentSpyTool {
    invocations: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl IncidentSpyTool {
    fn new() -> (Self, Arc<Mutex<Vec<serde_json::Value>>>) {
        let invocations = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                invocations: Arc::clone(&invocations),
            },
            invocations,
        )
    }
}

#[derive(schemars::JsonSchema, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct IncidentSpyArgs {
    #[allow(dead_code)]
    command: String,
    #[allow(dead_code)]
    label: String,
}

impl ylva::Tool for IncidentSpyTool {
    fn name(&self) -> &str {
        "incident_spy"
    }
    fn description(&self) -> &str {
        "Test-only spy: records its invocation and returns immediately."
    }
    fn input_schema(&self) -> schemars::Schema {
        ylva::tool_schema_for::<IncidentSpyArgs>()
    }
    fn needs_approval(&self, _arguments: &serde_json::Value) -> ylva::ApprovalCheck {
        ylva::ApprovalCheck::new(
            ylva::ApprovalVerdict::AutoApproved,
            ylva::DecisionMaker::Tool,
            ylva::RuleOrigin::Programmatic,
        )
    }
    fn timeout_secs(&self) -> u64 {
        30
    }
    fn non_default_redacted_arguments(
        &self,
        arguments: &serde_json::Value,
    ) -> Vec<(String, serde_json::Value)> {
        arguments
            .as_object()
            .map(|map| {
                map.iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }
    fn create_call(&self, id: uuid::Uuid, arguments: serde_json::Value) -> Box<dyn ylva::ToolCall> {
        Box::new(IncidentSpyCall {
            id,
            arguments,
            invocations: Arc::clone(&self.invocations),
        })
    }
}

struct IncidentSpyCall {
    id: uuid::Uuid,
    arguments: serde_json::Value,
    invocations: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl ylva::ToolCall for IncidentSpyCall {
    fn id(&self) -> uuid::Uuid {
        self.id
    }
    fn name(&self) -> &str {
        "incident_spy"
    }
    fn arguments(&self) -> &serde_json::Value {
        &self.arguments
    }
    fn timeout_secs(&self) -> u64 {
        30
    }
    fn non_default_redacted_arguments(&self) -> Vec<(String, serde_json::Value)> {
        Vec::new()
    }
    fn argument_order(&self, _key: &str) -> usize {
        usize::MAX
    }
    fn run(
        self: Box<Self>,
    ) -> futures::future::BoxFuture<'static, Result<Box<dyn ylva::ToolCallResult>, ylva::ToolError>>
    {
        Box::pin(async move {
            self.invocations
                .lock()
                .expect("incident spy mutex")
                .push(self.arguments.clone());
            Ok(
                Box::new(SimpleIncidentOutput("coverage reported".to_string()))
                    as Box<dyn ylva::ToolCallResult>,
            )
        })
    }
}

#[derive(Debug)]
struct SimpleIncidentOutput(String);

impl ylva::ToolCallResult for SimpleIncidentOutput {
    fn text(&self) -> String {
        self.0.clone()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn mixed_sse_reasoning_tool_call_reaches_agent_without_json_error() {
    let frames = incident_argument_frames("incident_spy");
    let server = MockLlmServer::start(MockLlmConfig {
        frames,
        end_after_frames: true,
        ..Default::default()
    })
    .await
    .expect("bind failed");
    let (spy_tool, spy_invocations) = IncidentSpyTool::new();
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(spy_tool));

    let (mut agent, mut ui_rx) = AgentLoop::new(
        Arc::new(EngineImpl::new(
            Arc::new(openai_compatible_provider_for(&server)),
            common::test_engine_config(),
        )),
        Arc::new(registry),
    );
    agent
        .send_command(AgentCommand::Prompt("run the coverage gate".into()))
        .await;
    let handle = agent.run();

    let updates = tokio::time::timeout(Duration::from_secs(5), async {
        let mut updates = Vec::new();
        loop {
            let update = ui_rx.recv().await.expect("agent update channel closed");
            if let UiUpdate::Error { message, .. } = &update {
                panic!("agent stream error: {message}");
            }
            let done = matches!(&update, UiUpdate::ToolDone { name, .. } if name == "incident_spy");
            updates.push(update);
            if done {
                break updates;
            }
        }
    })
    .await
    .expect("tool completion deadline");

    // The mixed frame's reasoning text must arrive before tool events, and the
    // incident's incomplete-JSON error must not surface.
    let thinking: Vec<_> = updates
        .iter()
        .filter_map(|update| match update {
            UiUpdate::AppendThinking(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(thinking, vec!["."]);
    assert!(
        !updates
            .iter()
            .any(|update| matches!(update, UiUpdate::Error { .. })),
        "the incident's incomplete-tool-call JSON error must not appear: {updates:?}"
    );

    // Stop the agent after the first observed tool completion so the mock never
    // replays the same call on a continuation.
    agent
        .send_command(AgentCommand::Stop {
            reason: "test observed the first tool completion".to_string(),
        })
        .await;
    drop(agent);
    handle
        .await
        .expect("agent loop task panicked")
        .expect("agent loop stopped cleanly");

    let invocations = spy_invocations.lock().expect("incident spy mutex");
    assert_eq!(
        invocations.len(),
        1,
        "exactly one assembled tool call may execute: {invocations:?}"
    );
    assert_eq!(
        invocations[0],
        serde_json::json!({
            "command": "make test-coverage 2>&1 | tail -25",
            "label": "Run coverage test gate"
        }),
        "assembled arguments must be the incident's complete JSON"
    );

    let bodies = server
        .request_bodies()
        .iter()
        .filter_map(|body| serde_json::from_str::<serde_json::Value>(body).ok())
        .filter(|body| body.get("messages").is_some())
        .collect::<Vec<_>>();
    assert_eq!(bodies.len(), 1, "one chat request drives the incident");
    server.abort();
}

#[tokio::test]
async fn incident_frames_reach_both_public_providers() {
    for edenai in [false, true] {
        let server = MockLlmServer::start(MockLlmConfig {
            frames: incident_argument_frames("incident_spy"),
            end_after_frames: true,
            ..Default::default()
        })
        .await
        .expect("bind server");
        let provider: Arc<dyn Provider> = if edenai {
            edenai_provider_for(&server)
        } else {
            provider_for(&server)
        };
        let mut receiver = provider
            .chat(vec![], vec![], CancellationToken::new())
            .await
            .expect("chat");
        let events = tokio::time::timeout(Duration::from_secs(5), async {
            let mut events = Vec::new();
            loop {
                let event = receiver.recv().await.expect("stream closed before done");
                let done = matches!(event, StreamEvent::Done);
                events.push(event);
                if done {
                    break events;
                }
            }
        })
        .await
        .expect("stream deadline");
        assert_eq!(events.len(), 7, "{events:?}");
        assert!(matches!(&events[0], StreamEvent::Thinking(text) if text == "."));
        assert!(
            matches!(&events[1], StreamEvent::Delta(delta) if delta.tool_call_index == 0 && delta.tool_call_id.as_deref() == Some("chatcmpl-tool-86eed4350a9d87cd") && delta.function_name.as_deref() == Some("incident_spy") && delta.arguments_fragment.as_deref() == Some("{\"command\": \""))
        );
        for (event, fragment) in events[2..6].iter().zip([
            "make test-coverage ",
            "2>&1 | tail -25",
            "\", \"label\": ",
            "\"Run coverage test gate\"}",
        ]) {
            assert!(
                matches!(event, StreamEvent::Delta(delta) if delta.tool_call_index == 0 && delta.tool_call_id.is_none() && delta.function_name.is_none() && delta.arguments_fragment.as_deref() == Some(fragment)),
                "{event:?}"
            );
        }
        assert!(matches!(events[6], StreamEvent::Done));
        server.abort();
    }
}

#[tokio::test]
async fn incomplete_arguments_error_without_spy_invocation() {
    let server = MockLlmServer::start(MockLlmConfig {
        frames: vec![mixed_reasoning_tool_call_frame(
            0,
            "incident_spy",
            "{\"command\": \"unfinished",
            ".",
        )],
        end_after_frames: true,
        ..Default::default()
    })
    .await
    .expect("bind server");
    let (spy, invocations) = IncidentSpyTool::new();
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(spy));
    let (mut agent, mut updates) = AgentLoop::new(
        Arc::new(EngineImpl::new(
            provider_for(&server),
            common::test_engine_config(),
        )),
        Arc::new(registry),
    );
    let handle = agent.run();
    agent
        .send_command(AgentCommand::Prompt("test".into()))
        .await;
    let error = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match updates.recv().await.expect("channel closed") {
                UiUpdate::Error { message, .. } => break message,
                UiUpdate::ToolDone { .. } => panic!("incomplete tool was executed"),
                _ => {}
            }
        }
    })
    .await
    .expect("error deadline");
    assert!(
        error.contains("stream ended with incomplete tool-call JSON"),
        "{error}"
    );
    assert!(invocations.lock().expect("spy mutex").is_empty());
    drop(agent);
    handle.await.expect("agent task").expect("agent stopped");
    server.abort();
}
