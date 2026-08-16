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

use futures::future::BoxFuture;
use schemars::schema::RootSchema;
use schemars::{schema_for, JsonSchema};
use serde::Deserialize;
use serde_json::Value;
use tempfile::TempDir;
use tokio::sync::mpsc;

use ylva::{
    default_registry, AgentCommand, AgentLoop, DeltaChunk, Message, MockEvent, MockProvider, Tool,
    ToolError, ToolRegistry, UiUpdate, MAX_ITERATIONS,
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
    predicate: F,
) -> Vec<UiUpdate>
where
    F: Fn(&UiUpdate) -> bool,
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
            if let UiUpdate::ToolPending { id, name } = &update {
                let needs_approval = registry.get(name).is_some_and(|tool| tool.needs_approval());
                let command = if needs_approval {
                    AgentCommand::DenyTool { id: *id }
                } else {
                    AgentCommand::ApproveTool { id: *id }
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

#[tokio::test(start_paused = true)]
async fn tool_call_pauses_for_approval_then_reprompts_after_result() {
    let dir = TempDir::new().expect("tempdir");
    let file = scratch_file(&dir, "a.txt", "hello tool");
    let registry = Arc::new(default_registry());
    let interval = Duration::from_millis(5);
    let provider = MockProvider::with_script(
        vec![call("read", 0, serde_json::json!({ "path": file }))],
        interval,
    );
    let (mut agent, mut ui_rx) = AgentLoop::new(Arc::new(provider), Arc::clone(&registry));

    agent
        .send_command(AgentCommand::Prompt("read it".into()))
        .await;
    let handle = agent.run();
    let _ = collect_until_advancing(&mut ui_rx, interval, is_stream_started).await;

    // The deltas + Done assemble into a tool call; the loop pauses at
    // ToolPending and must NOT re-prompt without approval.
    let pending = collect_until_advancing(&mut ui_rx, interval, is_tool_pending).await;
    assert!(matches!(pending.last(), Some(UiUpdate::ToolPending { name, .. }) if name == "read"));
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
        Some(UiUpdate::ToolStart { name, .. }) if name == "read"
    ));
    assert!(matches!(
        after.iter().find(|u| matches!(u, UiUpdate::ToolDone { .. })),
        Some(UiUpdate::ToolDone { result, .. }) if result.contains("hello tool")
    ));

    drop(agent);
    let context = handle.await.expect("loop ok").expect("context");
    assert!(matches!(
        context.messages().iter().find(|m| matches!(m, Message::ToolResult { .. })),
        Some(Message::ToolResult { name, result, .. }) if name == "read" && result.contains("hello tool")
    ));
}

#[tokio::test(start_paused = true)]
async fn two_tool_calls_drain_serially_with_single_reprompt() {
    let dir = TempDir::new().expect("tempdir");
    let a = scratch_file(&dir, "a.txt", "AAAA");
    let b = scratch_file(&dir, "b.txt", "BBBB");
    let registry = Arc::new(default_registry());
    let interval = Duration::from_millis(5);
    let provider = MockProvider::with_script(
        vec![
            call("read", 0, serde_json::json!({ "path": a })),
            call("read", 1, serde_json::json!({ "path": b })),
        ],
        interval,
    );
    let (mut agent, mut ui_rx) = AgentLoop::new(Arc::new(provider), Arc::clone(&registry));

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
    let mut results: Vec<&str> = context
        .messages()
        .iter()
        .filter_map(|m| match m {
            Message::ToolResult { result, .. } => Some(result.as_str()),
            _ => None,
        })
        .collect();
    results.sort();
    assert_eq!(results, ["AAAA", "BBBB"]);
}

#[tokio::test(start_paused = true)]
async fn failing_tool_emits_tool_error_and_still_reprompts() {
    let registry = Arc::new(default_registry());
    let interval = Duration::from_millis(5);
    let provider = MockProvider::with_script(
        vec![call(
            "read",
            0,
            serde_json::json!({ "path": "/nonexistent/never.txt" }),
        )],
        interval,
    );
    let (mut agent, mut ui_rx) = AgentLoop::new(Arc::new(provider), Arc::clone(&registry));

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
    let registry = Arc::new(default_registry());
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
    let (mut agent, mut ui_rx) = AgentLoop::new(Arc::new(provider), Arc::clone(&registry));

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
    let a = scratch_file(&dir, "a.txt", "A");
    let b = scratch_file(&dir, "b.txt", "B");
    let registry = Arc::new(default_registry());
    let interval = Duration::from_millis(5);
    let provider = MockProvider::with_script(
        vec![
            call("read", 0, serde_json::json!({ "path": a })),
            call("read", 1, serde_json::json!({ "path": b })),
        ],
        interval,
    );
    let (mut agent, mut ui_rx) = AgentLoop::new(Arc::new(provider), Arc::clone(&registry));

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
    agent.send_command(AgentCommand::Stop).await;
    assert_quiescent(&mut ui_rx, interval).await;

    drop(agent);
    let context = handle.await.expect("loop ok").expect("context");
    assert!(!context
        .messages()
        .iter()
        .any(|m| matches!(m, Message::ToolResult { .. })));
}

#[derive(JsonSchema, Deserialize)]
struct SleepArgs {}

/// A tool that never finishes, used to exercise kill-on-Stop.
struct SleepTool;

impl Tool for SleepTool {
    fn name(&self) -> &str {
        "sleep"
    }
    fn description(&self) -> &str {
        "sleeps almost forever"
    }
    fn schema(&self) -> RootSchema {
        schema_for!(SleepArgs)
    }
    fn needs_approval(&self) -> bool {
        false
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
    fn run<'a>(&'a self, _arguments: Value) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_secs(600)).await;
            Ok("done".to_string())
        })
    }
}

#[tokio::test(start_paused = true)]
async fn stop_mid_tool_kills_tool_and_returns_to_idle() {
    let mut registry = default_registry();
    registry.register(Arc::new(SleepTool));
    let registry = Arc::new(registry);
    let interval = Duration::from_millis(5);
    let provider =
        MockProvider::with_script(vec![call("sleep", 0, serde_json::json!({}))], interval);
    let (mut agent, mut ui_rx) = AgentLoop::new(Arc::new(provider), Arc::clone(&registry));

    agent.send_command(AgentCommand::Prompt("nap".into())).await;
    let handle = agent.run();
    let _ = collect_until_advancing(&mut ui_rx, interval, is_stream_started).await;
    let pending = collect_until_advancing(&mut ui_rx, interval, is_tool_pending).await;
    let id = match pending.last().unwrap() {
        UiUpdate::ToolPending { id, .. } => *id,
        _ => unreachable!(),
    };

    agent.send_command(AgentCommand::ApproveTool { id }).await;
    let started = collect_until_advancing(&mut ui_rx, interval, |u| {
        matches!(u, UiUpdate::ToolStart { .. })
    })
    .await;
    assert!(matches!(started.last(), Some(UiUpdate::ToolStart { name, .. }) if name == "sleep"));

    // Hard interrupt mid-tool: cancelled task reports ToolKilled, loop is idle.
    agent.send_command(AgentCommand::Stop).await;
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
    let registry = Arc::new(default_registry());
    let interval = Duration::from_millis(5);
    let provider = MockProvider::with_script(
        vec![call(
            "write",
            0,
            serde_json::json!({ "path": "x", "content": "y" }),
        )],
        interval,
    );
    let (mut agent, mut ui_rx) = AgentLoop::new(Arc::new(provider), Arc::clone(&registry));

    agent
        .send_command(AgentCommand::Prompt("loop".into()))
        .await;
    let handle = agent.run();

    let done = drive_until(&agent, &mut ui_rx, registry.as_ref(), interval, |u| {
        matches!(u, UiUpdate::Error(_))
    })
    .await;

    assert!(
        done.iter()
            .any(|u| matches!(u, UiUpdate::Error(e) if e.contains("iterations"))),
        "guard must emit an explanation error"
    );
    let starts = done
        .iter()
        .filter(|u| matches!(u, UiUpdate::StreamStarted))
        .count();
    assert_eq!(starts, MAX_ITERATIONS);

    drop(agent);
    let context = handle.await.expect("loop ok").expect("context");
    assert!(context
        .messages()
        .iter()
        .any(|m| matches!(m, Message::ToolError { name, .. } if name == "write")));
}

fn is_stream_started(update: &UiUpdate) -> bool {
    matches!(update, UiUpdate::StreamStarted)
}

fn is_tool_pending(update: &UiUpdate) -> bool {
    matches!(update, UiUpdate::ToolPending { .. })
}
