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

#[test]
fn user_visible_tool_failures_are_written_to_the_log() {
    let log = capture_logs(async {
        let script = vec![MockEvent::Delta(DeltaChunk {
            tool_call_index: 0,
            tool_call_id: Some("call-1".to_string()),
            function_name: Some("missing_tool".to_string()),
            arguments_fragment: Some("{}".to_string()),
        })];
        let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
            Arc::new(MockProvider::with_script(script, Duration::from_millis(0))),
            EngineConfig {
                provider_connection: "mock".to_string(),
                model: "test-model".to_string(),
                temperature: None,
                context_length: None,
                api_base_url: None,
                api_key: None,
                reasoning: None,

                compact_target_ratio: 0.5,
                soft_compaction_threshold: 0.7,
                hard_compaction_threshold: 0.9,
            },
        ));
        let (mut agent, mut ui_rx) = AgentLoop::new(engine, Arc::new(ToolRegistry::new()));
        let handle = agent.run();
        agent
            .send_command(AgentCommand::Prompt("trigger tool failure".to_string()))
            .await;

        loop {
            let update = ui_rx
                .recv()
                .await
                .expect("agent should emit a tool error update");
            if matches!(update, UiUpdate::ToolError { .. }) {
                break;
            }
        }

        drop(agent);
        handle
            .await
            .expect("agent task should not panic")
            .expect("agent should stop cleanly after the channel closes");
    });

    assert!(
        log.contains("tool failed"),
        "user-visible tool failures must be logged: {log}"
    );
    assert!(
        log.contains("Unknown tool 'missing_tool'"),
        "the log must include the tool failure diagnostic: {log}"
    );
}

#[test]
fn tool_execution_log_events_carry_spanned_tool_call_id() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let write_path = dir.path().join("out.txt");
    let log = capture_logs(async {
        let id = Uuid::new_v4();
        let (agent_tx, mut agent_rx) = mpsc::channel(8);
        let registry = tools::default_registry(
            std::sync::Arc::new(std::sync::RwLock::new(crate::sandbox::Sandbox::new())),
            tools::ToolRegistryConfig::default(),
        )
        .expect("valid sandbox fixture");
        let pending = PendingTool {
            call: registry
                .create_call(
                    "write",
                    id,
                    serde_json::json!({
                        "path": write_path.display().to_string(),
                        // Deliberately not lowercase-hex-only: a `Uuid::new_v4()`
                        // (used below as `tool_call_id`) is rendered into the log
                        // and consists solely of lowercase hex digits and hyphens,
                        // so a hex-only content literal (e.g. "abc") can rarely but
                        // genuinely collide with a random id substring and false-
                        // positive the "must not log content" assertion below.
                        "content": "XYZ",
                    }),
                )
                .expect("write tool registered"),
        };
        let _cancel = pending.execute(agent_tx, 0, None);
        tokio::time::timeout(Duration::from_secs(2), async {
            while let Some((_generation, event)) = agent_rx.recv().await {
                if matches!(event, AgentEvent::ToolDone { .. }) {
                    break;
                }
            }
        })
        .await
        .expect("tool completes in time");
    });
    let contents = log;
    assert!(
        contents.contains("tool_execute{tool=write"),
        "tool span with its tool field must be rendered: {contents}"
    );
    assert!(
        contents.contains("tool_call_id="),
        "tool call id must be attached to the tool's log events: {contents}"
    );
    assert!(
        contents.contains("wrote file"),
        "the write tool's log event must be inside the tool span: {contents}"
    );
    assert!(
        !contents.contains("XYZ"),
        "the write tool must not log the written content: {contents}"
    );
    let written = std::fs::read_to_string(&write_path).expect("read written file");
    assert_eq!(written, "XYZ");
}
