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

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use schemars::schema::RootSchema;
use schemars::{schema_for, JsonSchema};
use serde::Deserialize;
use serde_json::Value;
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use ylva::{
    default_registry, AgentCommand, AgentLoop, ApprovalCheck, AssistantStatus, DeltaChunk,
    EngineImpl, Message, MockEvent, MockProvider, Provider, Sandbox, StreamEvent, Tool, ToolCall,
    ToolCallResult, ToolError, ToolRegistry, ToolRegistryConfig, ToolSchema, UiUpdate,
    UserMessageKind, MAX_TOOL_USE_ITERATIONS,
};

/// Build a `MockEvent::Delta` carrying a complete tool call for `index`.
fn call(name: &str, index: usize, args: Value) -> MockEvent {
    MockEvent::Delta(DeltaChunk {
        tool_call_index: index,
        tool_call_id: None,
        function_name: Some(name.to_string()),
        arguments_fragment: Some(args.to_string()),
    })
}

/// Collect updates, advancing fake time between receives so a paused mock can
/// push its stream (interval-sleep gated) out, until `predicate` matches.
async fn collect_until_advancing<F>(
    ui_rx: &mut mpsc::Receiver<UiUpdate>,
    interval: Duration,
    mut predicate: F,
) -> Vec<UiUpdate>
where
    F: FnMut(&UiUpdate) -> bool,
{
    let mut collected = Vec::new();
    loop {
        if let Ok(update) = ui_rx.try_recv() {
            collected.push(update.clone());
            if predicate(&update) {
                return collected;
            }
        } else {
            tokio::time::advance(interval).await;
            tokio::task::yield_now().await;
        }
        if collected.len() > 500 {
            panic!("collect_until_advancing budget exceeded");
        }
    }
}

/// Like [`collect_until_advancing`], but auto-responds to every `ToolPending`
/// with the CLI-only driver policy (approve read/glob, deny write).
async fn drive_until<F>(
    agent: &AgentLoop,
    ui_rx: &mut mpsc::Receiver<UiUpdate>,
    registry: &ToolRegistry,
    interval: Duration,
    predicate: F,
) -> Vec<UiUpdate>
where
    F: Fn(&UiUpdate) -> bool,
{
    let mut collected = Vec::new();
    loop {
        if let Ok(update) = ui_rx.try_recv() {
            if let UiUpdate::ToolPending {
                id,
                name,
                arguments,
                ..
            } = &update
            {
                let approval_check = registry
                    .get(name)
                    .map(|tool| tool.needs_approval(arguments))
                    .unwrap_or(ApprovalCheck::UserApprovalRequired);
                let command = if matches!(approval_check, ApprovalCheck::AutoApproved) {
                    AgentCommand::ApproveTool { id: *id }
                } else {
                    AgentCommand::DenyTool {
                        id: *id,
                        reason: "denied in test".into(),
                    }
                };
                agent.send_command(command).await;
            }
            collected.push(update.clone());
            if predicate(&update) {
                return collected;
            }
        } else {
            tokio::time::advance(interval).await;
            tokio::task::yield_now().await;
        }
        if collected.len() > 500 {
            panic!("drive_until budget exceeded");
        }
    }
}

/// Advance fake time a few steps, then assert nothing new arrived (used to
/// check the loop is paused / back to idle).
async fn assert_quiescent(ui_rx: &mut mpsc::Receiver<UiUpdate>, interval: Duration) {
    for _ in 0..5 {
        tokio::time::advance(interval).await;
        tokio::task::yield_now().await;
    }
    assert!(ui_rx.try_recv().is_err(), "expected no further updates");
}

fn scratch_file(dir: &TempDir, name: &str, content: &str) -> String {
    let path = dir.path().join(name);
    std::fs::write(&path, content).expect("scratch write");
    path.display().to_string()
}

fn make_sandbox() -> std::sync::Arc<std::sync::RwLock<Sandbox>> {
    std::sync::Arc::new(std::sync::RwLock::new(Sandbox::new()))
}

struct SequencedMockProvider {
    phases: Vec<Vec<MockEvent>>,
    interval: Duration,
    calls: AtomicUsize,
}

impl SequencedMockProvider {
    fn new(phases: Vec<Vec<MockEvent>>, interval: Duration) -> Self {
        Self {
            phases,
            interval,
            calls: AtomicUsize::new(0),
        }
    }
}

impl Provider for SequencedMockProvider {
    fn chat<'a>(
        &'a self,
        _messages: Vec<Message>,
        _tools: Vec<ToolSchema>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        let phase_index = self.calls.fetch_add(1, Ordering::SeqCst);
        let script = self
            .phases
            .get(phase_index)
            .or_else(|| self.phases.last())
            .cloned()
            .unwrap_or_default();
        let interval = self.interval;
        Box::pin(async move {
            let (tx, rx) = mpsc::channel(16);
            tokio::spawn(async move {
                let last_index = script.len().saturating_sub(1);
                for (index, event) in script.into_iter().enumerate() {
                    let stream_event = match event {
                        MockEvent::Text(text) => StreamEvent::Text(text),
                        MockEvent::Thinking(text) => StreamEvent::Thinking(text),
                        MockEvent::Delta(delta) => StreamEvent::Delta(delta),
                    };
                    if tx.send(stream_event).await.is_err() {
                        return;
                    }
                    if index == last_index {
                        break;
                    }
                    tokio::select! {
                        _ = cancel.cancelled() => {
                            let _ = tx.send(StreamEvent::Cancelled).await;
                            return;
                        }
                        _ = tokio::time::sleep(interval) => {}
                    }
                }
                let _ = tx.send(StreamEvent::Done).await;
            });
            Ok(rx)
        })
    }

    fn name(&self) -> &str {
        "sequenced-mock"
    }

    fn list_models(&self) -> BoxFuture<'_, anyhow::Result<Vec<ylva::ModelInfo>>> {
        Box::pin(async { Ok(vec![]) })
    }
}

#[tokio::test(start_paused = true)]
async fn tool_call_pauses_for_approval_then_reprompts_after_result() {
    // Phase 6.0: reads are auto-approved for in-workspace files and auto-denied for
    // files outside the workspace. Writes always require explicit user approval
    // (UserApprovalRequired). This test uses `write` to exercise the ToolPending flow.
    let dir = TempDir::new().expect("tempdir");
    let output_path = dir.path().join("out.txt").to_string_lossy().to_string();
    let sandbox = std::sync::Arc::new(std::sync::RwLock::new(Sandbox {
        workspace_root: dir.path().to_path_buf(),
    }));
    let registry = Arc::new(default_registry(sandbox, ToolRegistryConfig::default()));
    let interval = Duration::from_millis(5);
    let provider = MockProvider::with_script(
        vec![call(
            "write",
            0,
            serde_json::json!({ "path": output_path, "content": "hello tool" }),
        )],
        interval,
    );
    let (mut agent, mut ui_rx) = AgentLoop::new(
        Arc::new(EngineImpl::new(
            Arc::new(provider),
            common::test_engine_config(),
        )),
        Arc::clone(&registry),
    );

    agent
        .send_command(AgentCommand::Prompt("write it".into()))
        .await;
    let handle = agent.run();
    let _ = collect_until_advancing(&mut ui_rx, interval, is_stream_started).await;

    // The write tool requires user approval (UserApprovalRequired); the loop pauses
    // at ToolPending and must NOT re-prompt without approval.
    let pending = collect_until_advancing(&mut ui_rx, interval, is_tool_pending).await;
    assert!(matches!(pending.last(), Some(UiUpdate::ToolPending { name, .. }) if name == "write"));
    assert_quiescent(&mut ui_rx, interval).await;

    // Approve, then a fresh re-prompt stream follows a ToolStart + ToolDone.
    let id = match pending.last().unwrap() {
        UiUpdate::ToolPending { id, .. } => *id,
        _ => unreachable!(),
    };
    agent.send_command(AgentCommand::ApproveTool { id }).await;
    let after = collect_until_advancing(&mut ui_rx, interval, is_stream_started).await;
    assert!(matches!(
        after.first(),
        Some(UiUpdate::ToolStart { name, .. }) if name == "write"
    ));
    assert!(matches!(
        after.iter().find(|u| matches!(u, UiUpdate::ToolDone { .. })),
        Some(UiUpdate::ToolDone { result_section, .. }) if result_section.content.contains("wrote")
    ));

    drop(agent);
    let context = handle.await.expect("loop ok").expect("context");
    assert!(matches!(
        context.messages().iter().find(|m| matches!(m, Message::ToolResult { .. })),
        Some(Message::ToolResult { value })
            if value.name == "write" && value.result.contains("wrote")
    ));
}

#[tokio::test(start_paused = true)]
async fn stopping_pending_tool_emits_idle_after_tool_error() {
    let registry = Arc::new(default_registry(
        make_sandbox(),
        ToolRegistryConfig::default(),
    ));
    let interval = Duration::from_millis(5);
    let provider = MockProvider::with_script(
        vec![call(
            "write",
            0,
            serde_json::json!({ "path": "out.txt", "content": "hello tool" }),
        )],
        interval,
    );
    let (mut agent, mut ui_rx) = AgentLoop::new(
        Arc::new(EngineImpl::new(
            Arc::new(provider),
            common::test_engine_config(),
        )),
        Arc::clone(&registry),
    );

    agent
        .send_command(AgentCommand::Prompt("write it".into()))
        .await;
    let handle = agent.run();
    let pending = collect_until_advancing(&mut ui_rx, interval, is_tool_pending).await;
    let pending_id = match pending.last() {
        Some(UiUpdate::ToolPending { id, .. }) => *id,
        other => panic!("expected a pending tool update, got {other:?}"),
    };

    agent
        .send_command(AgentCommand::Stop {
            reason: "aborted by user".into(),
        })
        .await;
    let stopped = collect_until_advancing(&mut ui_rx, interval, |update| {
        matches!(update, UiUpdate::AgentIdle)
    })
    .await;
    let error_index = stopped
        .iter()
        .position(|update| matches!(update, UiUpdate::ToolError { id, .. } if *id == pending_id))
        .expect("stopping a pending tool must emit its terminal error");
    let idle_index = stopped
        .iter()
        .position(|update| matches!(update, UiUpdate::AgentIdle))
        .expect("stopping a pending tool must emit an idle boundary");
    assert!(error_index < idle_index);

    drop(agent);
    handle
        .await
        .expect("loop task should finish")
        .expect("context");
}

#[tokio::test(start_paused = true)]
async fn two_tool_calls_drain_serially_with_single_reprompt() {
    let dir = TempDir::new().expect("tempdir");
    let a = scratch_file(&dir, "a.txt", "AAAA");
    let b = scratch_file(&dir, "b.txt", "BBBB");
    // Phase 6.0: reads inside the workspace are auto-approved. Use the tmpdir as
    // the workspace root so both scratch files are within the sandbox.
    let sandbox = std::sync::Arc::new(std::sync::RwLock::new(Sandbox {
        workspace_root: dir.path().to_path_buf(),
    }));
    let registry = Arc::new(default_registry(sandbox, ToolRegistryConfig::default()));
    let interval = Duration::from_millis(5);
    let provider = MockProvider::with_script(
        vec![
            call("read", 0, serde_json::json!({ "path": a })),
            call("read", 1, serde_json::json!({ "path": b })),
        ],
        interval,
    );
    let (mut agent, mut ui_rx) = AgentLoop::new(
        Arc::new(EngineImpl::new(
            Arc::new(provider),
            common::test_engine_config(),
        )),
        Arc::clone(&registry),
    );

    agent
        .send_command(AgentCommand::Prompt("read both".into()))
        .await;
    let handle = agent.run();
    let _ = collect_until_advancing(&mut ui_rx, interval, is_stream_started).await;

    // Drive through both approvals to the single re-prompt.
    let done = drive_until(
        &agent,
        &mut ui_rx,
        registry.as_ref(),
        interval,
        is_stream_started,
    )
    .await;

    // First call's ToolDone precedes the second call's ToolStart (serial).
    let first_done = done
        .iter()
        .position(|u| matches!(u, UiUpdate::ToolDone { .. }))
        .expect("a ToolDone for the first call");
    assert!(
        done[first_done + 1..]
            .iter()
            .any(|u| matches!(u, UiUpdate::ToolStart { .. })),
        "the second call must start only after the first ToolDone"
    );
    // Exactly one re-prompt: the single StreamStarted that terminated the drive.
    assert_eq!(
        done.iter()
            .filter(|u| matches!(u, UiUpdate::StreamStarted))
            .count(),
        1
    );

    drop(agent);
    let context = handle.await.expect("loop ok").expect("context");
    let messages = context.messages();

    // Tool results carry only the raw text now.
    let mut results: Vec<&str> = messages
        .iter()
        .filter_map(|m| match m {
            Message::ToolResult { value } => Some(value.result.as_str()),
            _ => None,
        })
        .collect();
    results.sort();
    assert_eq!(results, vec!["AAAA", "BBBB"]);

    // Metadata is now embedded in the ToolResult envelope, not separate messages.
    let results: Vec<&Message> = messages
        .iter()
        .filter(|m| matches!(m, Message::ToolResult { .. }))
        .collect();
    assert_eq!(results.len(), 2);
    for message in results {
        if let Message::ToolResult { value } = message {
            let metadata = value
                .result_metadata
                .as_ref()
                .expect("read result must have metadata");
            assert_eq!(metadata["read_bytes"], 4);
            assert_eq!(metadata["read_lines"], 1);
        }
    }
}

#[tokio::test(start_paused = true)]
async fn failing_tool_emits_tool_error_and_still_reprompts() {
    let registry = Arc::new(default_registry(
        make_sandbox(),
        ToolRegistryConfig::default(),
    ));
    let interval = Duration::from_millis(5);
    let provider = MockProvider::with_script(
        vec![call(
            "read",
            0,
            serde_json::json!({ "path": "nonexistent_file_that_does_not_exist.txt" }),
        )],
        interval,
    );
    let (mut agent, mut ui_rx) = AgentLoop::new(
        Arc::new(EngineImpl::new(
            Arc::new(provider),
            common::test_engine_config(),
        )),
        Arc::clone(&registry),
    );

    agent
        .send_command(AgentCommand::Prompt("read missing".into()))
        .await;
    let handle = agent.run();
    let _ = collect_until_advancing(&mut ui_rx, interval, is_stream_started).await;

    let done = drive_until(
        &agent,
        &mut ui_rx,
        registry.as_ref(),
        interval,
        is_stream_started,
    )
    .await;
    assert!(
        done.iter().any(|u| matches!(u, UiUpdate::ToolError { .. })),
        "a failing tool must surface a ToolError update"
    );
    assert_eq!(
        done.iter()
            .filter(|u| matches!(u, UiUpdate::StreamStarted))
            .count(),
        1,
        "a failing tool still re-prompts"
    );

    drop(agent);
    let context = handle.await.expect("loop ok").expect("context");
    assert!(context
        .messages()
        .iter()
        .any(|m| matches!(m, Message::ToolError { name, .. } if name == "read")));
}

#[tokio::test(start_paused = true)]
async fn denied_write_is_fed_back_and_triggers_a_new_request() {
    let dir = TempDir::new().expect("tempdir");
    let file = scratch_file(&dir, "a.txt", "content");
    let registry = Arc::new(default_registry(
        make_sandbox(),
        ToolRegistryConfig::default(),
    ));
    let interval = Duration::from_millis(5);
    let provider = MockProvider::with_script(
        vec![
            call("read", 0, serde_json::json!({ "path": file })),
            call(
                "write",
                1,
                serde_json::json!({ "path": "out.txt", "content": "x" }),
            ),
        ],
        interval,
    );
    let (mut agent, mut ui_rx) = AgentLoop::new(
        Arc::new(EngineImpl::new(
            Arc::new(provider),
            common::test_engine_config(),
        )),
        Arc::clone(&registry),
    );

    agent
        .send_command(AgentCommand::Prompt("do it".into()))
        .await;
    let handle = agent.run();
    let _ = collect_until_advancing(&mut ui_rx, interval, is_stream_started).await;

    let done = drive_until(
        &agent,
        &mut ui_rx,
        registry.as_ref(),
        interval,
        is_stream_started,
    )
    .await;

    // The denial is a ToolError followed by a re-prompt (model sees it), not an
    // immediate return to Idle.
    let error_idx = done
        .iter()
        .position(|u| matches!(u, UiUpdate::ToolError { .. }))
        .expect("denial surfaces as ToolError");
    assert!(
        done[error_idx + 1..]
            .iter()
            .any(|u| matches!(u, UiUpdate::StreamStarted)),
        "a StreamStarted re-prompt must follow the denial"
    );

    drop(agent);
    let context = handle.await.expect("loop ok").expect("context");
    assert!(context
        .messages()
        .iter()
        .any(|m| matches!(m, Message::ToolError { name, .. } if name == "write")));
}

#[tokio::test(start_paused = true)]
async fn stop_while_awaiting_approval_clears_pending_tools() {
    let dir = TempDir::new().expect("tempdir");
    let a = dir.path().join("a.txt").to_string_lossy().to_string();
    let b = dir.path().join("b.txt").to_string_lossy().to_string();
    // Phase 6.0: writes always require user approval (UserApprovalRequired), so
    // both calls will produce ToolPending.  Use tmpdir as workspace root so
    // the sandbox does not auto-deny the paths.
    let sandbox = std::sync::Arc::new(std::sync::RwLock::new(Sandbox {
        workspace_root: dir.path().to_path_buf(),
    }));
    let registry = Arc::new(default_registry(sandbox, ToolRegistryConfig::default()));
    let interval = Duration::from_millis(5);
    let provider = MockProvider::with_script(
        vec![
            call("write", 0, serde_json::json!({ "path": a, "content": "A" })),
            call("write", 1, serde_json::json!({ "path": b, "content": "B" })),
        ],
        interval,
    );
    let (mut agent, mut ui_rx) = AgentLoop::new(
        Arc::new(EngineImpl::new(
            Arc::new(provider),
            common::test_engine_config(),
        )),
        Arc::clone(&registry),
    );

    agent.send_command(AgentCommand::Prompt("go".into())).await;
    let handle = agent.run();
    let _ = collect_until_advancing(&mut ui_rx, interval, is_stream_started).await;

    let pending = collect_until_advancing(&mut ui_rx, interval, is_tool_pending).await;
    assert_eq!(
        pending
            .iter()
            .filter(|u| matches!(u, UiUpdate::ToolPending { .. }))
            .count(),
        1,
        "only the first call is offered for approval"
    );

    // Hard interrupt before approving either: pending tools are dropped.
    agent
        .send_command(AgentCommand::Stop {
            reason: "aborted by user".into(),
        })
        .await;

    // The agent emits ToolError for the approval-pending tool so the TUI
    // can clear its approval footer and mark the bubble as aborted.
    let after_stop = collect_until_advancing(&mut ui_rx, interval, is_tool_error).await;
    assert!(
        after_stop
            .iter()
            .any(|u| matches!(u, UiUpdate::ToolError { .. })),
        "expected ToolError after stop while awaiting approval"
    );

    drop(agent);
    let context = handle.await.expect("loop ok").expect("context");
    // Neither tool ran, so no ToolResult in history.
    assert!(!context
        .messages()
        .iter()
        .any(|m| matches!(m, Message::ToolResult { .. })));
    // Both pending tools must have a ToolError result so the model history
    // stays consistent (every tool call must have a corresponding result).
    let tool_error_count = context
        .messages()
        .iter()
        .filter(|m| matches!(m, Message::ToolError { .. }))
        .count();
    assert_eq!(tool_error_count, 2, "expected one error per pending tool");
}

#[derive(JsonSchema, Deserialize)]
struct SleepArgs {}

/// Minimal [`ToolCallResult`] wrapper for a plain string, used in test tools that
/// do not need to expose domain properties.
#[derive(Debug)]
struct SimpleStringOutput(String);

impl ToolCallResult for SimpleStringOutput {
    fn text(&self) -> String {
        self.0.clone()
    }
}

struct SleepToolCall {
    id: uuid::Uuid,
    arguments: serde_json::Value,
}

impl ToolCall for SleepToolCall {
    fn id(&self) -> uuid::Uuid {
        self.id
    }
    fn name(&self) -> &str {
        "sleep"
    }
    fn arguments(&self) -> &serde_json::Value {
        &self.arguments
    }
    fn timeout_secs(&self) -> u64 {
        600
    }
    fn non_default_redacted_arguments(&self) -> Vec<(String, serde_json::Value)> {
        vec![]
    }
    fn argument_order(&self, _key: &str) -> usize {
        usize::MAX
    }
    fn run(
        self: Box<Self>,
    ) -> futures::future::BoxFuture<'static, Result<Box<dyn ToolCallResult>, ToolError>> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_secs(600)).await;
            let output: Box<dyn ToolCallResult> = Box::new(SimpleStringOutput("done".to_string()));
            Ok(output)
        })
    }
}

/// A tool that never finishes, used to exercise kill-on-Stop.
struct SleepTool;

impl Tool for SleepTool {
    fn name(&self) -> &str {
        "sleep"
    }
    fn description(&self) -> &str {
        "sleeps almost forever"
    }
    fn input_schema(&self) -> RootSchema {
        schema_for!(SleepArgs)
    }
    fn needs_approval(&self, _args: &serde_json::Value) -> ApprovalCheck {
        ApprovalCheck::AutoApproved
    }
    fn timeout_secs(&self) -> u64 {
        600
    }
    fn non_default_redacted_arguments(&self, arguments: &Value) -> Vec<(String, Value)> {
        self.non_default_arguments(arguments)
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.clone()))
            .collect()
    }
    fn create_call(&self, id: uuid::Uuid, arguments: serde_json::Value) -> Box<dyn ToolCall> {
        Box::new(SleepToolCall { id, arguments })
    }
}

#[tokio::test(start_paused = true)]
async fn stop_mid_tool_kills_tool_and_returns_to_idle() {
    let mut registry = default_registry(make_sandbox(), ToolRegistryConfig::default());
    registry.register(Arc::new(SleepTool));
    let registry = Arc::new(registry);
    let interval = Duration::from_millis(5);
    let provider =
        MockProvider::with_script(vec![call("sleep", 0, serde_json::json!({}))], interval);
    let (mut agent, mut ui_rx) = AgentLoop::new(
        Arc::new(EngineImpl::new(
            Arc::new(provider),
            common::test_engine_config(),
        )),
        Arc::clone(&registry),
    );

    agent.send_command(AgentCommand::Prompt("nap".into())).await;
    let handle = agent.run();
    let _ = collect_until_advancing(&mut ui_rx, interval, is_stream_started).await;
    // Phase 6.0: SleepTool is AutoApproved — the loop starts it directly without
    // ToolPending. Collect until ToolStart arrives.
    let started = collect_until_advancing(&mut ui_rx, interval, |u| {
        matches!(u, UiUpdate::ToolStart { .. })
    })
    .await;
    assert!(matches!(started.last(), Some(UiUpdate::ToolStart { name, .. }) if name == "sleep"));

    // Hard interrupt mid-tool: cancelled task reports ToolKilled, loop is idle.
    agent
        .send_command(AgentCommand::Stop {
            reason: "aborted by user".into(),
        })
        .await;
    let killed = collect_until_advancing(&mut ui_rx, interval, |u| {
        matches!(u, UiUpdate::ToolKilled { .. })
    })
    .await;
    assert!(killed
        .iter()
        .any(|u| matches!(u, UiUpdate::ToolKilled { .. })));
    assert_quiescent(&mut ui_rx, interval).await;

    drop(agent);
    let context = handle.await.expect("loop ok").expect("context");
    assert!(!context
        .messages()
        .iter()
        .any(|m| matches!(m, Message::ToolResult { .. })));
}

#[tokio::test(start_paused = true)]
async fn iteration_guard_stops_after_max_reprompts() {
    let registry = Arc::new(default_registry(
        make_sandbox(),
        ToolRegistryConfig::default(),
    ));
    let interval = Duration::from_millis(5);
    let provider = MockProvider::with_script(
        vec![call(
            "write",
            0,
            serde_json::json!({ "path": "x", "content": "y" }),
        )],
        interval,
    );
    let (mut agent, mut ui_rx) = AgentLoop::new(
        Arc::new(EngineImpl::new(
            Arc::new(provider),
            common::test_engine_config(),
        )),
        Arc::clone(&registry),
    );

    agent
        .send_command(AgentCommand::Prompt("loop".into()))
        .await;
    let handle = agent.run();

    let done = drive_until(&agent, &mut ui_rx, registry.as_ref(), interval, |u| {
        matches!(u, UiUpdate::Error { .. })
    })
    .await;

    assert!(
        done.iter()
            .any(|u| matches!(u, UiUpdate::Error { message, run_id: None } if message.contains("iterations"))),
        "guard must emit an explanation error attributed to the root loop (run_id: None)"
    );
    let starts = done
        .iter()
        .filter(|u| matches!(u, UiUpdate::StreamStarted))
        .count();
    assert_eq!(starts, MAX_TOOL_USE_ITERATIONS);

    drop(agent);
    let context = handle.await.expect("loop ok").expect("context");
    assert!(context
        .messages()
        .iter()
        .any(|m| matches!(m, Message::ToolError { name, .. } if name == "write")));
}

#[tokio::test(start_paused = true)]
async fn intermediate_subagent_completion_does_not_reprompt_until_all_children_finish() {
    use std::sync::Mutex;

    use ylva::child_agent_manager::{ChildAgentManager, FinishResult, SpawnFn};

    let registry = Arc::new(default_registry(
        make_sandbox(),
        ToolRegistryConfig {
            include_spawn_subagent: true,
            auto_approve_spawn: true,
            ..ToolRegistryConfig::default()
        },
    ));
    let interval = Duration::from_millis(5);
    let provider = SequencedMockProvider::new(
        vec![
            vec![
                call(
                    "spawn_subagent",
                    0,
                    serde_json::json!({
                        "label": "First child",
                        "task": "first task"
                    }),
                ),
                call(
                    "spawn_subagent",
                    1,
                    serde_json::json!({
                        "label": "Second child",
                        "task": "second task"
                    }),
                ),
            ],
            vec![MockEvent::Text("working".to_string())],
            vec![MockEvent::Text("combined answer".to_string())],
        ],
        interval,
    );
    let (mut agent, mut ui_rx) = AgentLoop::new(
        Arc::new(EngineImpl::new(
            Arc::new(provider),
            common::test_engine_config(),
        )),
        Arc::clone(&registry),
    );

    let (finish_tx, finish_rx) = mpsc::channel(4);
    let (subagent_ui_tx, subagent_ui_rx) = mpsc::channel(16);
    let spawned_runs = Arc::new(Mutex::new(Vec::new()));
    let spawned_runs_clone = Arc::clone(&spawned_runs);
    let spawn_fn: SpawnFn = Box::new(move |request, _cancel, _ui_tx| {
        spawned_runs_clone
            .lock()
            .unwrap()
            .push((request.run_id, request.call_id, request.label));
        ylva::child_agent_manager::SpawnOutcome {
            command_sender: None,
            handle: tokio::spawn(async {}),
        }
    });
    let manager = ChildAgentManager::new(subagent_ui_tx, spawn_fn);
    agent = agent.with_child_agent_manager(manager, finish_rx, subagent_ui_rx);

    agent
        .send_command(AgentCommand::Prompt("delegate twice".to_string()))
        .await;
    let handle = agent.run();

    let mut stream_start_count = 0;
    let initial_updates = collect_until_advancing(&mut ui_rx, interval, |update| {
        if matches!(update, UiUpdate::StreamStarted) {
            stream_start_count += 1;
        }
        stream_start_count == 2 && matches!(update, UiUpdate::AppendText(text) if text == "working")
    })
    .await;
    let stream_starts_before_completion = initial_updates
        .iter()
        .filter(|update| matches!(update, UiUpdate::StreamStarted))
        .count();
    let (running_run_id, running_call_id) = initial_updates
        .iter()
        .find_map(|update| match update {
            UiUpdate::ChildAgentStarted {
                run_id, call_id, ..
            } => Some((*run_id, *call_id)),
            _ => None,
        })
        .expect("one child should have started");
    let (second_call_id, second_run_id) = initial_updates
        .iter()
        .find_map(|update| match update {
            UiUpdate::ToolProgress {
                id,
                title_arguments,
            } if title_arguments
                .iter()
                .any(|(key, value)| key == "status" && value == "queued") =>
            {
                let run_id = title_arguments
                    .iter()
                    .find(|(key, _)| key == "run_id")
                    .and_then(|(_, value)| value.as_str())
                    .and_then(|value| value.parse().ok())?;
                Some((*id, run_id))
            }
            _ => None,
        })
        .expect("one child should be queued");
    assert_ne!(running_call_id, second_call_id);
    assert_eq!(spawned_runs.lock().unwrap().len(), 1);

    finish_tx
        .send(FinishResult {
            run_id: running_run_id,
            call_id: running_call_id,
            status: "success".to_string(),
            result: Some("first result".to_string()),
            error: None,
            sources: vec![],
        })
        .await
        .expect("first child completion should be accepted");
    let first_done_updates = collect_until_advancing(&mut ui_rx, interval, |update| {
        matches!(
            update,
            UiUpdate::ToolDone { id, name, .. }
                if *id == running_call_id && name == "spawn_subagent"
        )
    })
    .await;

    let mut updates_after_first = first_done_updates;
    for _ in 0..5 {
        tokio::time::advance(interval).await;
        tokio::task::yield_now().await;
        while let Ok(update) = ui_rx.try_recv() {
            updates_after_first.push(update);
        }
    }
    assert!(
        !updates_after_first
            .iter()
            .any(|update| matches!(update, UiUpdate::StreamStarted)),
        "an intermediate completion must not start a parent continuation"
    );

    finish_tx
        .send(FinishResult {
            run_id: second_run_id,
            call_id: second_call_id,
            status: "success".to_string(),
            result: Some("second result".to_string()),
            error: None,
            sources: vec![],
        })
        .await
        .expect("second child completion should be accepted");
    let final_updates = collect_until_advancing(&mut ui_rx, interval, |update| {
        matches!(update, UiUpdate::StreamStarted)
    })
    .await;
    assert_eq!(
        final_updates
            .iter()
            .filter(|update| matches!(update, UiUpdate::StreamStarted))
            .count(),
        1,
        "the final completion should trigger one parent continuation"
    );
    assert_eq!(
        stream_starts_before_completion + 1,
        initial_updates
            .iter()
            .filter(|update| matches!(update, UiUpdate::StreamStarted))
            .count()
            + final_updates
                .iter()
                .filter(|update| matches!(update, UiUpdate::StreamStarted))
                .count(),
        "only the final completion may add a parent stream"
    );

    let _ = collect_until_advancing(&mut ui_rx, interval, |update| {
        matches!(update, UiUpdate::StreamDone)
    })
    .await;
    drop(agent);
    let context = handle.await.expect("loop ok").expect("context");
    let completion_notices: Vec<&str> = context
        .messages()
        .iter()
        .filter_map(|message| match message {
            Message::User {
                content,
                kind: UserMessageKind::ImplicitToolNote,
            } if content.contains("\"event\":\"subagent_completed\"") => Some(content.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(completion_notices.len(), 2);
    assert!(completion_notices[0].contains("\"all_done\":false"));
    assert!(completion_notices[1].contains("\"all_done\":true"));
    assert!(completion_notices
        .iter()
        .any(|notice| notice.contains("\"label\":\"First child\"")));
    assert!(completion_notices
        .iter()
        .any(|notice| notice.contains("\"label\":\"Second child\"")));
}

#[tokio::test(start_paused = true)]
async fn iteration_guard_also_stops_a_runaway_spawn_subagent_finish_reprompt_loop() {
    // Regression test: a child that resolves near-instantly (finishes via
    // `finish_tx` essentially as soon as it's spawned) drives the root
    // through the `finish_rx` "root was idle, forcing reprompt" branch
    // instead of `advance_tool_queue`'s queue-empty branch. That branch used
    // to call `start_request` directly with no iteration bookkeeping at all
    // — so a model that just keeps calling `spawn_subagent` again on every
    // finish notice (rather than ever answering) could reprompt forever,
    // one HTTP request per "finish", completely bypassing
    // `MAX_TOOL_USE_ITERATIONS`. The guard must trip here exactly like it
    // does for the plain `write`-in-a-loop case above.
    use tokio_util::sync::CancellationToken;
    use ylva::child_agent_manager::{ChildAgentManager, FinishResult, SpawnFn};

    let registry = Arc::new(default_registry(
        make_sandbox(),
        ToolRegistryConfig {
            include_spawn_subagent: true,
            auto_approve_spawn: true,
            ..ToolRegistryConfig::default()
        },
    ));
    let interval = Duration::from_millis(5);
    let provider = MockProvider::with_script(
        vec![call(
            "spawn_subagent",
            0,
            serde_json::json!({ "label": "Do it again", "task": "do it again" }),
        )],
        interval,
    );
    let (mut agent, mut ui_rx) = AgentLoop::new(
        Arc::new(EngineImpl::new(
            Arc::new(provider),
            common::test_engine_config(),
        )),
        Arc::clone(&registry),
    );

    let (finish_tx, finish_rx) = mpsc::channel(64);
    let (subagent_ui_tx, subagent_ui_rx) = mpsc::channel(64);
    // Resolves immediately: reports success via `finish_tx` right away and
    // never actually does any work, simulating a child that finishes near-
    // instantly relative to the root's own request/response cycle.
    let spawn_fn: SpawnFn = Box::new(move |request, _cancel: CancellationToken, _ui_tx| {
        let finish_tx = finish_tx.clone();
        let handle = tokio::spawn(async move {
            let _ = finish_tx
                .send(FinishResult {
                    run_id: request.run_id,
                    call_id: request.call_id,
                    status: "success".to_string(),
                    result: Some("done".to_string()),
                    error: None,
                    sources: vec![],
                })
                .await;
        });
        ylva::child_agent_manager::SpawnOutcome {
            command_sender: None,
            handle,
        }
    });
    let manager = ChildAgentManager::new(subagent_ui_tx, spawn_fn);
    agent = agent.with_child_agent_manager(manager, finish_rx, subagent_ui_rx);

    agent
        .send_command(AgentCommand::Prompt("delegate forever".into()))
        .await;
    let handle = agent.run();

    let done = drive_until(&agent, &mut ui_rx, registry.as_ref(), interval, |u| {
        matches!(u, UiUpdate::Error { .. })
    })
    .await;

    assert!(
        done.iter()
            .any(|u| matches!(u, UiUpdate::Error { message, run_id: None } if message.contains("iterations"))),
        "guard must trip and emit an explanation error attributed to the root loop \
         (run_id: None), got: {done:?}"
    );

    drop(agent);
    let context = handle.await.expect("loop ok").expect("context");
    assert!(
        matches!(
            context.messages().last(),
            Some(Message::Assistant {
                status: AssistantStatus::Interrupted,
                ..
            })
        ),
        "the guard tripping must be reported as an Interrupted assistant bubble, got: {:?}",
        context.messages()
    );
}

#[tokio::test(start_paused = true)]
async fn thinking_deltas_surface_separately_from_assistant_text() {
    // Thinking/reasoning fragments must never contaminate the assistant text
    // accumulator: they arrive via `UiUpdate::AppendThinking` and are absent
    // from the final `Message::Assistant` content.
    let registry = Arc::new(default_registry(
        make_sandbox(),
        ToolRegistryConfig::default(),
    ));
    let interval = Duration::from_millis(5);
    let provider = MockProvider::with_script(
        vec![
            MockEvent::Thinking("pondering...".into()),
            MockEvent::Thinking(" still pondering...".into()),
            MockEvent::Text("answer".into()),
        ],
        interval,
    );
    let (mut agent, mut ui_rx) = AgentLoop::new(
        Arc::new(EngineImpl::new(
            Arc::new(provider),
            common::test_engine_config(),
        )),
        Arc::clone(&registry),
    );

    agent
        .send_command(AgentCommand::Prompt("think it through".into()))
        .await;
    let handle = agent.run();
    let updates =
        collect_until_advancing(&mut ui_rx, interval, |u| matches!(u, UiUpdate::StreamDone)).await;

    let thinking_texts: Vec<&str> = updates
        .iter()
        .filter_map(|u| match u {
            UiUpdate::AppendThinking(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(thinking_texts, vec!["pondering...", " still pondering..."]);

    let assistant_texts: Vec<&str> = updates
        .iter()
        .filter_map(|u| match u {
            UiUpdate::AppendText(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(assistant_texts, vec!["answer"]);

    drop(agent);
    let context = handle.await.expect("loop ok").expect("context");
    assert!(matches!(
        context.messages().last(),
        Some(Message::Assistant { content, .. }) if content == "answer"
    ));
}

#[tokio::test(start_paused = true)]
async fn stop_while_spawn_subagent_is_running_resolves_it_to_cancelled_tool_done() {
    // Regression test for the ToolProgress redesign: `spawn_subagent`'s
    // "running" acknowledgment is now a non-terminal `ToolProgress`, so a
    // `Stop` command must still resolve the bubble to a real, terminal
    // `ToolDone` (status=cancelled) instead of leaving it stuck mid-flight.
    use tokio_util::sync::CancellationToken;
    use ylva::child_agent_manager::{ChildAgentManager, SpawnFn};

    let registry = Arc::new(default_registry(
        make_sandbox(),
        ToolRegistryConfig {
            include_spawn_subagent: true,
            auto_approve_spawn: true,
            ..ToolRegistryConfig::default()
        },
    ));
    let interval = Duration::from_millis(5);
    let provider = MockProvider::with_script(
        vec![call(
            "spawn_subagent",
            0,
            serde_json::json!({ "label": "Do the thing", "task": "do the thing", "briefing": "context" }),
        )],
        interval,
    );
    let (mut agent, mut ui_rx) = AgentLoop::new(
        Arc::new(EngineImpl::new(
            Arc::new(provider),
            common::test_engine_config(),
        )),
        Arc::clone(&registry),
    );

    let (finish_tx, finish_rx) = mpsc::channel(1);
    let (subagent_ui_tx, subagent_ui_rx) = mpsc::channel(8);
    // Never actually finishes on its own — only cancellation ends it, so the
    // test can observe `Stop` resolving the bubble rather than a real finish.
    let spawn_fn: SpawnFn = Box::new(move |_request, cancel: CancellationToken, _ui_tx| {
        let handle = tokio::spawn(async move {
            cancel.cancelled().await;
        });
        ylva::child_agent_manager::SpawnOutcome {
            command_sender: None,
            handle,
        }
    });
    let manager = ChildAgentManager::new(subagent_ui_tx, spawn_fn);
    agent = agent.with_child_agent_manager(manager, finish_rx, subagent_ui_rx);
    drop(finish_tx);

    agent
        .send_command(AgentCommand::Prompt("delegate it".into()))
        .await;
    let handle = agent.run();
    let _ = collect_until_advancing(&mut ui_rx, interval, is_stream_started).await;
    let started = collect_until_advancing(&mut ui_rx, interval, |u| {
        matches!(u, UiUpdate::ToolStart { .. })
    })
    .await;
    assert!(
        matches!(started.last(), Some(UiUpdate::ToolStart { name, .. }) if name == "spawn_subagent")
    );

    // The "running" acknowledgment now arrives as a non-terminal ToolProgress.
    let progressed = collect_until_advancing(&mut ui_rx, interval, |u| {
        matches!(u, UiUpdate::ToolProgress { .. })
    })
    .await;
    assert!(progressed
        .iter()
        .any(|u| matches!(u, UiUpdate::ToolProgress { .. })));

    agent
        .send_command(AgentCommand::Stop {
            reason: "aborted by user".into(),
        })
        .await;
    let done = collect_until_advancing(
        &mut ui_rx,
        interval,
        |u| matches!(u, UiUpdate::ToolDone { name, .. } if name == "spawn_subagent"),
    )
    .await;
    let tool_done = done
        .iter()
        .find(|u| matches!(u, UiUpdate::ToolDone { name, .. } if name == "spawn_subagent"))
        .expect("expected a spawn_subagent ToolDone after Stop");
    match tool_done {
        UiUpdate::ToolDone { summary, .. } => {
            assert!(
                summary
                    .iter()
                    .any(|(key, value)| key == "status" && value == "cancelled"),
                "expected status=cancelled in the ToolDone summary, got: {summary:?}"
            );
        }
        _ => unreachable!(),
    }

    // `spawn_subagent` resolves synchronously (unlike a real running tool
    // task), so the loop may already be reprompting again by the time Stop
    // lands — the mock provider script is deterministic and would keep
    // re-issuing the same call forever. Force termination here rather than
    // asserting quiescence.
    drop(agent);
    let context = handle.await.expect("loop ok").expect("context");
    let saw_cancelled_notice = context.messages().iter().any(|message| {
        matches!(
            message,
            Message::User { content, kind: UserMessageKind::ImplicitToolNote }
                if content.contains("spawn_subagent")
                    && content.contains("\"event\":\"subagent_completed\"")
                    && content.contains("\"status\":\"cancelled\"")
                    && content.contains("\"all_done\":true")
        )
    });
    assert!(
        saw_cancelled_notice,
        "expected a trailing implicit user message announcing the cancelled spawn_subagent \
         child, got: {:?}",
        context.messages()
    );
}

fn is_stream_started(update: &UiUpdate) -> bool {
    matches!(update, UiUpdate::StreamStarted)
}

fn is_tool_pending(update: &UiUpdate) -> bool {
    matches!(update, UiUpdate::ToolPending { .. })
}

fn is_tool_error(update: &UiUpdate) -> bool {
    matches!(update, UiUpdate::ToolError { .. })
}

#[tokio::test(start_paused = true)]
async fn for_child_targeting_an_unknown_run_id_is_dropped_without_hanging_the_root() {
    // Regression test for `AgentCommand::ForChild`'s "stale target" case: a
    // command addressed to a `run_id` the root's `ChildAgentManager` doesn't
    // recognize (already finished, or never existed) must be logged and
    // dropped, not panic or silently wedge the root loop's `select!` — the
    // root must stay fully responsive to its own, unrelated commands
    // afterwards.
    let registry = Arc::new(default_registry(
        make_sandbox(),
        ToolRegistryConfig::default(),
    ));
    let interval = Duration::from_millis(5);
    let provider = MockProvider::with_script(vec![MockEvent::Text("hi there".into())], interval);
    let (mut agent, mut ui_rx) = AgentLoop::new(
        Arc::new(EngineImpl::new(
            Arc::new(provider),
            common::test_engine_config(),
        )),
        Arc::clone(&registry),
    );

    // No `ChildAgentManager` is wired at all, so this is the strongest form
    // of "no live child recognizes this run_id".
    let stray_run_id = uuid::Uuid::new_v4();
    agent
        .send_command(AgentCommand::ForChild {
            run_id: stray_run_id,
            command: Box::new(AgentCommand::ApproveTool { id: stray_run_id }),
        })
        .await;

    let handle = agent.run();

    // The root must still be able to run a completely ordinary prompt
    // afterwards — if the stray `ForChild` had wedged the `select!` loop,
    // this would hang forever instead of completing.
    agent
        .send_command(AgentCommand::Prompt("hello".into()))
        .await;
    let updates = collect_until_advancing(
        &mut ui_rx,
        interval,
        |u| matches!(u, UiUpdate::AppendText(text) if text == "hi there"),
    )
    .await;
    assert!(
        updates
            .iter()
            .any(|u| matches!(u, UiUpdate::AppendText(text) if text == "hi there")),
        "expected the root to still process an ordinary prompt after a stale \
         ForChild, got: {updates:?}"
    );

    drop(agent);
    handle.await.expect("loop ok").expect("context");
}
