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

use super::*;

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
async fn stop_while_awaiting_approval_clears_pending_tools() {
    let dir = TempDir::new().expect("tempdir");
    let a = dir.path().join("a.txt").to_string_lossy().to_string();
    let b = dir.path().join("b.txt").to_string_lossy().to_string();
    // Phase 6.0: writes always require user approval (UserApprovalRequired), so
    // both calls will produce ToolPending. The temporary directory is the
    // sandbox root.
    let sandbox = std::sync::Arc::new(std::sync::RwLock::new(Sandbox {
        sandbox_root: dir.path().to_path_buf(),
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
