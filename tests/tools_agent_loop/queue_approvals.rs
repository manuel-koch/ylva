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
async fn tool_call_pauses_for_approval_then_reprompts_after_result() {
    // Phase 6.0: reads are auto-approved for in-workspace files; a path outside
    // the workspace (or a dot-path) requires user approval. Writes always
    // require explicit user approval (UserApprovalRequired). This test uses
    // `write` to exercise the ToolPending flow.
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
async fn base64_read_result_stays_in_model_context_but_not_tool_bubble() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("image.png");
    let bytes = [137, 80, 78, 71, 13, 10, 26, 10, 0, 255];
    std::fs::write(&path, bytes).expect("write fixture");
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    let sandbox = std::sync::Arc::new(std::sync::RwLock::new(Sandbox {
        workspace_root: dir.path().to_path_buf(),
    }));
    let registry = Arc::new(default_registry(sandbox, ToolRegistryConfig::default()));
    let interval = Duration::from_millis(5);
    let provider = MockProvider::with_script(
        vec![call(
            "read",
            0,
            serde_json::json!({
                "path": path.display().to_string(),
                "mode": "bytes",
                "encoding": "base64",
            }),
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
        .send_command(AgentCommand::Prompt("read the image".into()))
        .await;
    let handle = agent.run();
    let updates = collect_until_advancing(&mut ui_rx, interval, |update| {
        matches!(update, UiUpdate::ToolDone { .. })
    })
    .await;
    let result_section = updates
        .iter()
        .find_map(|update| match update {
            UiUpdate::ToolDone { result_section, .. } => Some(result_section),
            _ => None,
        })
        .expect("read must produce a ToolDone update");
    assert!(
        result_section.content.is_empty(),
        "opaque Base64 must not render in the tool bubble: {:?}",
        result_section.content
    );

    drop(agent);
    let context = handle.await.expect("loop ok").expect("context");
    let tool_result = context
        .messages()
        .iter()
        .find_map(|message| match message {
            Message::ToolResult { value } if value.name == "read" => Some(value),
            _ => None,
        })
        .expect("read result must be persisted");
    assert_eq!(tool_result.result, encoded);
    assert_eq!(
        tool_result.attachments,
        vec![ToolAttachment::Image {
            media_type: "image/png".to_string(),
        }]
    );
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
async fn bash_call_pending_carries_label_in_title_arguments() {
    // A bash call with a non-empty `label` must surface it in the pending
    // event's `title_arguments`, ordered `command` (primary) then `label`
    // (prop), so the collapsed bubble/CLI status line can show the purpose
    // while the call awaits approval — and the pending event's body
    // sections must lead with `Label` then `Command` for the expanded
    // bubble.
    let registry = Arc::new(default_registry(
        make_sandbox(),
        ToolRegistryConfig::default(),
    ));
    let interval = Duration::from_millis(5);
    let provider = MockProvider::with_script(
        vec![call(
            "bash",
            0,
            serde_json::json!({
                "command": "echo hi",
                "label": "Greet the user",
            }),
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
        .send_command(AgentCommand::Prompt("run it".into()))
        .await;
    let handle = agent.run();
    let _ = collect_until_advancing(&mut ui_rx, interval, is_stream_started).await;

    let pending = collect_until_advancing(&mut ui_rx, interval, is_tool_pending).await;
    let title_arguments = match pending.last() {
        Some(UiUpdate::ToolPending {
            name,
            title_arguments,
            ..
        }) => {
            assert_eq!(name, "bash");
            title_arguments.clone()
        }
        other => panic!("expected a bash ToolPending, got: {other:?}"),
    };
    assert_eq!(
        title_arguments,
        vec![
            ("command".to_string(), serde_json::json!("echo hi")),
            ("label".to_string(), serde_json::json!("Greet the user")),
        ],
        "pending event must carry command first, then the label prop"
    );
    let pending_content_sections = match pending.last().unwrap() {
        UiUpdate::ToolPending {
            content_sections, ..
        } => content_sections.clone(),
        other => panic!("expected a bash ToolPending, got: {other:?}"),
    };
    assert_eq!(
        pending_content_sections
            .iter()
            .map(|section| match section {
                ylva::agent::tools::Section::Content(content) => content.label.clone(),
                other => panic!("unexpected section: {other:?}"),
            })
            .collect::<Vec<_>>(),
        vec![Some("Label".to_string()), Some("Command".to_string())],
        "expanded pending body must lead with Label then Command"
    );

    agent
        .send_command(AgentCommand::Stop {
            reason: "test done".into(),
        })
        .await;
    let after_stop = collect_until_advancing(&mut ui_rx, interval, is_tool_error).await;
    let error_content_sections = after_stop
        .iter()
        .find_map(|update| match update {
            UiUpdate::ToolError {
                content_sections, ..
            } => Some(content_sections.clone()),
            _ => None,
        })
        .expect("expected ToolError after stop while awaiting approval");
    assert_eq!(
        error_content_sections
            .iter()
            .map(|section| match section {
                ylva::agent::tools::Section::Content(content) => content.label.clone(),
                other => panic!("unexpected section: {other:?}"),
            })
            .collect::<Vec<_>>(),
        vec![Some("Label".to_string()), Some("Command".to_string())],
        "ToolError must keep the pending Label/Command sections for replay"
    );

    drop(agent);
    handle.await.expect("loop ok").expect("context");
}

#[tokio::test(start_paused = true)]
async fn bash_tool_done_content_sections_lead_with_label_and_command() {
    // Approving the call runs it; the completed bubble's body must lead
    // with `Label` then `Command`, followed by the command output.
    let registry = Arc::new(default_registry(
        make_sandbox(),
        ToolRegistryConfig::default(),
    ));
    let interval = Duration::from_millis(5);
    let provider = MockProvider::with_script(
        vec![call(
            "bash",
            0,
            serde_json::json!({
                "command": "echo hi",
                "label": "Greet the user",
            }),
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
        .send_command(AgentCommand::Prompt("run it".into()))
        .await;
    let handle = agent.run();
    let _ = collect_until_advancing(&mut ui_rx, interval, is_stream_started).await;

    let pending = collect_until_advancing(&mut ui_rx, interval, is_tool_pending).await;
    let pending_id = match pending.last() {
        Some(UiUpdate::ToolPending { id, .. }) => *id,
        other => panic!("expected a bash ToolPending, got: {other:?}"),
    };
    agent
        .send_command(AgentCommand::ApproveTool { id: pending_id })
        .await;

    let after = collect_until_advancing(&mut ui_rx, interval, |update| {
        matches!(update, UiUpdate::ToolDone { .. })
    })
    .await;
    let done_content_sections = after
        .iter()
        .find_map(|update| match update {
            UiUpdate::ToolDone {
                content_sections, ..
            } => Some(content_sections.clone()),
            _ => None,
        })
        .expect("expected ToolDone");
    assert_eq!(done_content_sections.len(), 3);
    let rendered_labels = done_content_sections
        .iter()
        .map(|section| match section {
            ylva::agent::tools::Section::Content(content) => content.label.clone(),
            other => panic!("unexpected section: {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        rendered_labels,
        vec![
            Some("Label".to_string()),
            Some("Command".to_string()),
            Some("Result".to_string())
        ],
        "completed body must be Label, Command, then Result"
    );
    match &done_content_sections[2] {
        ylva::agent::tools::Section::Content(content) => {
            assert!(
                content.content.contains("hi"),
                "output section must carry the command output: {}",
                content.content
            );
        }
        other => panic!("unexpected section: {other:?}"),
    }

    drop(agent);
    handle.await.expect("loop ok").expect("context");
}

#[tokio::test(start_paused = true)]
async fn ten_page_batch_preserves_continuity_after_a_successful_summary() {
    assert_ten_page_batch_continues(true).await;
}

#[tokio::test(start_paused = true)]
async fn ten_page_batch_preserves_continuity_after_a_failed_summary() {
    assert_ten_page_batch_continues(false).await;
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
    assert_eq!(starts, DEFAULT_MAX_TOOL_USE_ITERATIONS);

    drop(agent);
    let context = handle.await.expect("loop ok").expect("context");
    assert!(context
        .messages()
        .iter()
        .any(|m| matches!(m, Message::ToolError { name, .. } if name == "write")));
}

#[tokio::test(start_paused = true)]
async fn iteration_guard_honors_a_custom_root_limit() {
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
    let (agent, mut ui_rx) = AgentLoop::new(
        Arc::new(EngineImpl::new(
            Arc::new(provider),
            common::test_engine_config(),
        )),
        Arc::clone(&registry),
    );
    let mut agent = agent.with_max_tool_iterations(3);

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
            .any(|u| matches!(u, UiUpdate::Error { message, run_id: None } if message.contains("Max tool-use iterations (3)"))),
        "the guard must report the configured limit, got: {done:?}"
    );
    let starts = done
        .iter()
        .filter(|u| matches!(u, UiUpdate::StreamStarted))
        .count();
    assert_eq!(
        starts, 3,
        "a custom root limit replaces the default {DEFAULT_MAX_TOOL_USE_ITERATIONS}"
    );

    drop(agent);
    let context = handle.await.expect("loop ok").expect("context");
    assert!(matches!(
        context.messages().last(),
        Some(Message::Assistant {
            status: AssistantStatus::Interrupted,
            ..
        })
    ));
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
