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
fn unknown_only_batch_counts_once_and_respects_iteration_cap() {
    crate::test_support::run_locked(async {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
            Arc::new(ToolResultTrackingProvider {
                requests: Arc::clone(&requests),
                request_received: Arc::new(tokio::sync::Notify::new()),
                reproduce_unknown_tool_overlap: true,
            }),
            test_engine().config(),
        ));
        let (agent_loop, mut ui_rx) = AgentLoop::new(engine, Arc::new(ToolRegistry::new()));
        let mut agent_loop = agent_loop.with_max_tool_iterations(1);
        let handle = agent_loop.run();
        agent_loop
            .send_command(AgentCommand::Prompt("Run the tools.".to_string()))
            .await;
        let mut unknown_tool_errors = 0;
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                match ui_rx.recv().await.expect("agent channel stays open") {
                    UiUpdate::ToolError { .. } => unknown_tool_errors += 1,
                    UiUpdate::Error { message, .. } => {
                        assert!(message.contains("Max tool-use iterations (1) exceeded."));
                        break;
                    }
                    _ => {}
                }
            }
        })
        .await
        .expect("all-unknown batch reaches iteration cap");
        drop(agent_loop);
        let context = tokio::time::timeout(Duration::from_secs(3), handle)
            .await
            .expect("agent stops")
            .expect("agent task succeeds")
            .expect("root returns context");
        assert_eq!(unknown_tool_errors, 3);
        assert_eq!(requests.lock().expect("requests lock").len(), 1);
        assert_eq!(context.messages().iter().filter(|message| matches!(message, Message::Assistant { tool_calls, .. } if !tool_calls.is_empty())).count(), 1);
    });
}

#[test]
fn unknown_tool_does_not_leave_bash_approval_idle() {
    crate::test_support::run_locked(async {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let request_received = Arc::new(tokio::sync::Notify::new());
        let release_read = Arc::new(tokio::sync::Notify::new());
        let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
            Arc::new(ToolResultTrackingProvider {
                requests: Arc::clone(&requests),
                request_received: Arc::clone(&request_received),
                reproduce_unknown_tool_overlap: true,
            }),
            EngineConfig {
                provider_connection: "tool-result-tracking".to_string(),
                model: "test-model".to_string(),
                temperature: None,
                context_length: None,
                api_base_url: None,
                api_key: None,
                reasoning: None,
                compact_target_ratio: 0.4,
                soft_compaction_threshold: 0.5,
                hard_compaction_threshold: 0.8,
            },
        ));
        let mut registry =
            tools::default_registry(test_sandbox(), tools::ToolRegistryConfig::default());
        registry.register(Arc::new(GatedReadTool {
            inner: tools::read::ReadTool::new(SandboxContext::new(test_sandbox())),
            release: Arc::clone(&release_read),
        }));
        let (mut agent_loop, mut ui_rx) = AgentLoop::new(engine, Arc::new(registry));
        let handle = agent_loop.run();
        agent_loop
            .send_command(AgentCommand::Prompt("Run the tools.".to_string()))
            .await;
        request_received.notified().await;
        let mut premature_follow_up = false;
        let bash_id = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                match ui_rx.recv().await.expect("agent channel stays open") {
                    UiUpdate::ToolStart { name, .. } if name == "read" => {
                        premature_follow_up = tokio::time::timeout(
                            Duration::from_millis(100),
                            request_received.notified(),
                        )
                        .await
                        .is_ok();
                        if !premature_follow_up {
                            release_read.notify_one();
                        }
                    }
                    UiUpdate::ToolPending { id, name, .. } if name == "bash" => break id,
                    _ => {}
                }
            }
        })
        .await
        .expect("bash approval arrives");
        if premature_follow_up {
            release_read.notify_one();
            tokio::time::timeout(Duration::from_secs(3), async {
                while !matches!(ui_rx.recv().await, Some(UiUpdate::StreamDone)) {}
            })
            .await
            .expect("premature stream finishes while bash awaits approval");
        }
        agent_loop
            .send_command(AgentCommand::ApproveTool { id: bash_id })
            .await;
        let completed = tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if matches!(ui_rx.recv().await, Some(UiUpdate::ToolDone { id, .. }) if id == bash_id) {
                        break;
                    }
                }
            }).await.is_ok();
        drop(agent_loop);
        let _ = tokio::time::timeout(Duration::from_secs(3), handle).await;
        assert!(
            completed,
            "approved bash must execute rather than be ignored in Idle"
        );
        let requests = requests.lock().expect("requests lock");
        assert_eq!(requests.len(), 3, "one follow-up per completed tool batch");
        assert_eq!(
            requests[1]
                .iter()
                .filter(
                    |message| matches!(message, Message::ToolError { name, .. } if name == "grep")
                )
                .count(),
            2,
            "unknown-tool results reach the model in the same completed batch",
        );
        assert!(requests[1].iter().any(
            |message| matches!(message, Message::ToolResult { value } if value.name == "read")
        ));
        assert!(requests[2].iter().any(
            |message| matches!(message, Message::ToolResult { value } if value.name == "bash")
        ));
    });
}

#[test]
fn large_read_result_is_capped_to_remaining_context_before_follow_up_request() {
    crate::test_support::run_locked(async {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let request_received = Arc::new(tokio::sync::Notify::new());
        let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
            Arc::new(ToolResultTrackingProvider {
                requests: Arc::clone(&requests),
                request_received: Arc::clone(&request_received),
                reproduce_unknown_tool_overlap: false,
            }),
            EngineConfig {
                provider_connection: "tool-result-tracking".to_string(),
                model: "test-model".to_string(),
                temperature: None,
                context_length: Some(10_000),
                api_base_url: None,
                api_key: None,
                reasoning: None,
                compact_target_ratio: 0.4,
                soft_compaction_threshold: 0.5,
                hard_compaction_threshold: 0.8,
            },
        ));
        let registry = Arc::new(tools::default_registry(
            test_sandbox(),
            tools::ToolRegistryConfig {
                allowed_tools: vec![tools::read::ReadTool::NAME.to_string()],
                auto_approve: std::collections::HashSet::from([tools::AutoApproveTool::Read]),
                ..Default::default()
            },
        ));
        let tool_schemas = registry.schemas();
        let (agent_loop, mut ui_rx) = AgentLoop::new(engine, registry);
        let mut agent_loop =
            agent_loop.with_compaction_engine(test_engine_with_context_length(10_000));
        let handle = agent_loop.run();
        agent_loop
            .send_command(AgentCommand::Prompt(
                "Read the requested source file.".to_string(),
            ))
            .await;

        let first_context_usage = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                // The startup baseline (`ModelView`) arrives first; only the
                // per-request estimate is compared against the sent request.
                if let UiUpdate::ContextUsage {
                    estimated_tokens,
                    context_length,
                    source: ContextUsageSource::SerializedRequest,
                } = ui_rx.recv().await.expect("agent update channel stays open")
                {
                    break (estimated_tokens, context_length);
                }
            }
        })
        .await
        .expect("request estimate arrives before stream events");
        request_received.notified().await;
        let expected_first_usage = {
            let first_request = requests
                .lock()
                .expect("request tracking lock is not poisoned")[0]
                .clone();
            let estimator = crate::llm::TokenEstimator::Tiktoken;
            first_request
                .iter()
                .map(|message| estimate_message_tokens(estimator, message))
                .sum::<usize>()
                + tool_schema_token_count(&tool_schemas)
        };
        assert_eq!(first_context_usage, (expected_first_usage, 10_000));

        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if requests
                    .lock()
                    .expect("request tracking lock is not poisoned")
                    .len()
                    >= 2
                {
                    break;
                }
                request_received.notified().await;
            }
        })
        .await
        .expect("follow-up request starts after the tool result is committed");

        let second_request = requests
            .lock()
            .expect("request tracking lock is not poisoned")[1]
            .clone();
        let read_result = second_request.iter().find_map(|message| match message {
            Message::ToolResult { value } if value.name == tools::read::ReadTool::NAME => {
                Some(value)
            }
            _ => None,
        });
        let read_result = read_result.expect("bounded read result remains in the model view");
        let result_token_budget = 10_000usize
            .saturating_sub(10_000 / 10)
            .saturating_sub(10_000 / 20)
            .saturating_sub(512)
            .saturating_sub(first_context_usage.0);
        assert!(
            crate::llm::TokenEstimator::Tiktoken.estimate_tokens(&read_result.result)
                <= result_token_budget,
            "read result fits its execution-time output budget"
        );
        let estimate_messages = |messages: &[Message]| {
            let estimator = crate::llm::TokenEstimator::Tiktoken;
            messages
                .iter()
                .map(|message| estimate_message_tokens(estimator, message))
                .sum::<usize>()
                + tool_schema_token_count(&tool_schemas)
        };
        let expected_usage = estimate_messages(&second_request);
        let first_request = requests
            .lock()
            .expect("request tracking lock is not poisoned")[0]
            .clone();
        let initial_usage = estimate_messages(&first_request);
        let mut observed_usage = Vec::new();
        while observed_usage.last().copied() != Some(expected_usage) {
            let update = tokio::time::timeout(Duration::from_secs(3), ui_rx.recv())
                .await
                .expect("context usage arrives before the follow-up request")
                .expect("agent update channel stays open");
            if let UiUpdate::ContextUsage {
                estimated_tokens, ..
            } = update
            {
                observed_usage.push(estimated_tokens);
            }
        }
        assert!(
                observed_usage.contains(&expected_usage) && expected_usage < 10_000,
                "footer reflects the bounded follow-up request: initial={initial_usage}, after={observed_usage:?}"
            );
        drop(agent_loop);
        let context = tokio::time::timeout(Duration::from_secs(3), handle)
            .await
            .expect("agent loop terminates after drop")
            .expect("agent task joins")
            .expect("agent completes");
        let canonical_result = context
            .messages()
            .iter()
            .find_map(|message| match message {
                Message::ToolResult { value } if value.name == tools::read::ReadTool::NAME => {
                    Some(&value.result)
                }
                _ => None,
            })
            .expect("canonical read remains in the transcript");
        assert!(
            crate::llm::TokenEstimator::Tiktoken.estimate_tokens(canonical_result)
                > result_token_budget,
            "the source fixture must actually exceed the result allowance"
        );
    });
}

#[tokio::test]
async fn advance_tool_queue_emits_clarify_pending_for_clarify_tool() {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ClarifyTool));
    let registry = Arc::new(registry);
    let call_id = Uuid::new_v4();
    let mut pending_tools = VecDeque::from([PendingTool {
        call: registry
            .create_call(
                "clarify",
                call_id,
                serde_json::json!({
                    "question": "Pick one",
                    "choices": ["a", "b"],
                }),
            )
            .expect("clarify tool registered"),
    }]);
    let (ui_tx, mut ui_rx) = mpsc::channel(4);
    let (agent_tx, _agent_rx) = mpsc::channel(1);
    let mut context = Context::new();
    let mut mode = AgentMode::ToolPending;
    let mut current_cancel = None;
    let mut current_tool_cancel = None;
    let mut iterations = 1usize;
    let max_tool_iterations = DEFAULT_MAX_TOOL_USE_ITERATIONS;
    let mut clarify_started_at = None;
    let mut pending_steers = VecDeque::new();

    advance_tool_queue(
        &mut pending_tools,
        &mut context,
        &mut mode,
        &mut current_cancel,
        &mut current_tool_cancel,
        &mut iterations,
        max_tool_iterations,
        false,
        test_engine(),
        registry,
        ui_tx,
        agent_tx,
        0,
        &mut clarify_started_at,
        &mut pending_steers,
        &mut None,
        &mut HashMap::new(),
        None,
        &mut false,
        &mut None,
        None,
        None,
    )
    .await;

    assert_eq!(mode, AgentMode::ClarifyPending);
    assert!(matches!(
        ui_rx.recv().await,
        Some(UiUpdate::ClarifyPending {
            id,
            question,
            context: _,
            choices: Some(choices),
            run_id: _,
        }) if id == call_id && question == "Pick one" && choices == vec!["a".to_string(), "b".to_string()]
    ));
}

#[tokio::test]
async fn advance_tool_queue_emits_tool_pending_for_non_clarify_tool() {
    let sandbox = test_sandbox();
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(WriteTool::new(SandboxContext::new(sandbox))));
    let registry = Arc::new(registry);
    let call_id = Uuid::new_v4();
    let mut pending_tools = VecDeque::from([PendingTool {
        call: registry
            .create_call(
                "write",
                call_id,
                serde_json::json!({
                    "path": "Cargo.toml",
                    "content": "ignored",
                }),
            )
            .expect("write tool registered"),
    }]);
    let (ui_tx, mut ui_rx) = mpsc::channel(4);
    let (agent_tx, _agent_rx) = mpsc::channel(1);
    let mut context = Context::new();
    let mut mode = AgentMode::ToolPending;
    let mut current_cancel = None;
    let mut current_tool_cancel = None;
    let mut iterations = 1usize;
    let max_tool_iterations = DEFAULT_MAX_TOOL_USE_ITERATIONS;
    let mut clarify_started_at = None;
    let mut pending_steers = VecDeque::new();

    advance_tool_queue(
        &mut pending_tools,
        &mut context,
        &mut mode,
        &mut current_cancel,
        &mut current_tool_cancel,
        &mut iterations,
        max_tool_iterations,
        false,
        test_engine(),
        registry,
        ui_tx,
        agent_tx,
        0,
        &mut clarify_started_at,
        &mut pending_steers,
        &mut None,
        &mut HashMap::new(),
        None,
        &mut false,
        &mut None,
        None,
        None,
    )
    .await;

    assert_eq!(mode, AgentMode::ToolPending);
    assert!(matches!(
        ui_rx.recv().await,
        Some(UiUpdate::ToolPending { id, name, .. }) if id == call_id && name == "write"
    ));
}

#[tokio::test]
async fn advance_tool_queue_injects_pending_steers_before_starting_next_request() {
    // Regression test: queued steers (pending user messages typed while
    // tools were running) must reach the model on the very next request
    // once all tool results for this round are in context, rather than
    // waiting for a full tool-free turn to complete via StreamDone.
    let registry = Arc::new(ToolRegistry::new());
    let mut pending_tools: VecDeque<PendingTool> = VecDeque::new();
    let (ui_tx, mut ui_rx) = mpsc::channel(4);
    let (agent_tx, _agent_rx) = mpsc::channel(1);
    let mut context = Context::new();
    context.push_tool_result(ToolResult {
        id: Uuid::new_v4(),
        name: "write".to_string(),
        result: "ok".to_string(),
        attachments: vec![],
        result_section_id: None,
        result_metadata: None,
        result_metadata_schema: None,
        summary: vec![],
        content_sections: vec![],
        duration: std::time::Duration::ZERO,
    });
    let mut mode = AgentMode::ToolPending;
    let mut current_cancel = None;
    let mut current_tool_cancel = None;
    let mut iterations = 1usize;
    let max_tool_iterations = DEFAULT_MAX_TOOL_USE_ITERATIONS;
    let mut clarify_started_at = None;
    let mut pending_steers = VecDeque::from(["steered while tool ran".to_string()]);

    advance_tool_queue(
        &mut pending_tools,
        &mut context,
        &mut mode,
        &mut current_cancel,
        &mut current_tool_cancel,
        &mut iterations,
        max_tool_iterations,
        true,
        test_engine(),
        registry,
        ui_tx,
        agent_tx,
        0,
        &mut clarify_started_at,
        &mut pending_steers,
        &mut None,
        &mut HashMap::new(),
        None,
        &mut false,
        &mut None,
        None,
        None,
    )
    .await;

    assert_eq!(mode, AgentMode::Streaming);
    assert!(pending_steers.is_empty(), "steer must be drained");
    assert_eq!(
        context.messages().last(),
        Some(&Message::User {
            content: "steered while tool ran".to_string(),
            kind: UserMessageKind::Explicit,
        })
    );
    assert!(matches!(
        ui_rx.recv().await,
        Some(UiUpdate::SteerApplied { .. })
    ));
}

#[tokio::test]
async fn a_budgeted_call_without_a_useful_allowance_is_skipped_not_executed() {
    let registry = Arc::new(tools::default_registry(
        test_sandbox(),
        tools::ToolRegistryConfig::default(),
    ));
    let call_id = Uuid::new_v4();
    let mut pending_tools = VecDeque::from([PendingTool {
        call: registry
            .create_call("read", call_id, serde_json::json!({ "path": "Cargo.toml" }))
            .expect("read tool registered"),
    }]);
    let (ui_tx, mut ui_rx) = mpsc::channel(8);
    let (agent_tx, _agent_rx) = mpsc::channel(8);
    let mut context = Context::new();
    let mut mode = AgentMode::ToolPending;
    let mut iterations = 0usize;

    advance_tool_queue(
        &mut pending_tools,
        &mut context,
        &mut mode,
        &mut None,
        &mut None,
        &mut iterations,
        DEFAULT_MAX_TOOL_USE_ITERATIONS,
        false,
        test_engine_with_context_length(100),
        registry,
        ui_tx,
        agent_tx,
        0,
        &mut None,
        &mut VecDeque::new(),
        &mut None,
        &mut HashMap::new(),
        None,
        &mut false,
        &mut None,
        None,
        None,
    )
    .await;

    assert!(pending_tools.is_empty());
    assert_eq!(iterations, 1, "a skipped round still consumes an iteration");
    assert!(matches!(
        context.messages().last(),
        Some(Message::ToolError { id, error, .. })
            if *id == call_id && error.contains("not executed")
    ));
    assert!(matches!(
        ui_rx.recv().await,
        Some(UiUpdate::ToolAutoDenied { id, .. }) if id == call_id
    ));
}

#[test]
fn controlled_batch_caps_measure_round_trips_skips_and_task_continuity() {
    crate::test_support::run_locked(async {
        for (batch_cap, operation_count, expected_requests, expected_skips) in [
            (6, 0, 1, 0),
            (6, 1, 2, 0),
            (6, 6, 2, 0),
            (6, 7, 3, 1),
            (6, 12, 3, 6),
            (6, 10, 3, 4),
            (4, 10, 4, 8),
        ] {
            let requests = Arc::new(Mutex::new(Vec::new()));
            let engine = Arc::new(EngineImpl::new(
                Arc::new(ResubmittingBatchProvider {
                    operation_count,
                    requests: requests.clone(),
                    skipped_write_path: None,
                }),
                test_engine().config(),
            ));
            let registry = Arc::new(tools::default_registry(
                test_sandbox(),
                tools::ToolRegistryConfig::default(),
            ));
            let (mut agent, mut updates) = AgentLoop::new(engine, registry);
            agent.test_batch_cap = batch_cap;
            let handle = agent.run();
            agent
                .send_command(AgentCommand::Prompt("retain this task".to_string()))
                .await;
            let mut started = 0;
            let mut skipped = 0;
            let mut lossy = 0;
            tokio::time::timeout(Duration::from_secs(10), async {
                while let Some(update) = updates.recv().await {
                    match update {
                        UiUpdate::ToolStart { .. } => started += 1,
                        UiUpdate::ToolAutoDenied { .. } => skipped += 1,
                        UiUpdate::Notice { text, .. } if text.contains("lossily") => lossy += 1,
                        UiUpdate::Error { message, .. } => panic!("batch failed: {message}"),
                        UiUpdate::AppendText(text) if text == "complete" => break,
                        _ => {}
                    }
                }
            })
            .await
            .expect("batch completes");
            assert_eq!(started, operation_count);
            assert_eq!(skipped, expected_skips);
            assert_eq!(lossy, 0);
            {
                let recorded = requests.lock().expect("batch requests");
                assert_eq!(recorded.len(), expected_requests);
                assert!(recorded
                    .iter()
                    .all(|request| request.iter().any(|message| matches!(
                        message, Message::User { content, kind: UserMessageKind::Explicit }
                            if content == "retain this task"
                    ))));
            }
            drop(agent);
            handle.await.expect("batch joins").expect("batch succeeds");
        }
    });
}

#[test]
fn invalid_denied_and_unknown_slots_do_not_backfill_or_execute_an_excess_write() {
    crate::test_support::run_locked(async {
        let directory = tempfile::tempdir().expect("directory");
        let write_path = directory.path().join("skipped.txt");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let engine = Arc::new(EngineImpl::new(
            Arc::new(ResubmittingBatchProvider {
                operation_count: 8,
                requests: requests.clone(),
                skipped_write_path: Some(write_path.display().to_string()),
            }),
            test_engine().config(),
        ));
        let registry = Arc::new(tools::default_registry(
            test_sandbox(),
            tools::ToolRegistryConfig::default(),
        ));
        let (mut agent, mut updates) = AgentLoop::new(engine, registry);
        let handle = agent.run();
        agent
            .send_command(AgentCommand::Prompt("perform the batch".to_string()))
            .await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(update) = updates.recv().await {
                match update {
                    UiUpdate::ToolPending { id, .. } => {
                        agent
                            .send_command(AgentCommand::DenyTool {
                                id,
                                reason: "deliberate denial".to_string(),
                            })
                            .await;
                    }
                    UiUpdate::AppendText(text) if text == "complete" => break,
                    UiUpdate::Error { message, .. } => panic!("batch failed: {message}"),
                    _ => {}
                }
            }
        })
        .await
        .expect("mixed batch terminates");
        assert!(!write_path.exists(), "excess write never executes");
        {
            let recorded = requests.lock().expect("requests");
            assert_eq!(recorded.len(), 2);
            let continuation = &recorded[1];
            assert_eq!(
                continuation
                    .iter()
                    .filter(|message| matches!(message, Message::ToolResult { .. }))
                    .count(),
                3
            );
            assert_eq!(
                continuation
                    .iter()
                    .filter(|message| matches!(message, Message::ToolError { .. }))
                    .count(),
                5
            );
            assert!(continuation.iter().any(|message| matches!(message,
                Message::ToolError { name, error, .. } if name == "unknown" && error.contains("not executed")
            )));
            assert!(continuation.iter().any(|message| matches!(message,
                Message::ToolError { name, error, .. } if name == "write" && error.contains("not executed")
            )));
        }
        drop(agent);
        handle.await.expect("joins").expect("context");
    });
}

#[test]
fn only_the_first_six_calls_of_a_response_are_executed() {
    crate::test_support::run_locked(async {
        let script: Vec<MockEvent> = (0..8)
            .map(|index| {
                MockEvent::Delta(DeltaChunk {
                    tool_call_index: index,
                    tool_call_id: Some(format!("call_{index}")),
                    function_name: Some("read".to_string()),
                    arguments_fragment: Some(
                        serde_json::json!({ "path": "Cargo.toml" }).to_string(),
                    ),
                })
            })
            .collect();
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
        let registry = Arc::new(tools::default_registry(
            test_sandbox(),
            tools::ToolRegistryConfig::default(),
        ));
        let (mut agent_loop, mut ui_rx) = AgentLoop::new(engine, registry);
        let handle = agent_loop.run();
        agent_loop
            .send_command(AgentCommand::Prompt("go".into()))
            .await;

        let mut starts = 0;
        let mut skipped = 0;
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(update) = ui_rx.recv().await {
                match update {
                    UiUpdate::ToolStart { .. } => starts += 1,
                    UiUpdate::ToolAutoDenied { reason, .. } => {
                        assert!(reason.contains("smaller batches"));
                        skipped += 1;
                    }
                    UiUpdate::StreamDone if skipped > 0 => break,
                    _ => {}
                }
                if skipped >= 2 && starts >= 6 {
                    break;
                }
            }
        })
        .await
        .expect("first round resolves");
        assert_eq!(skipped, 2, "calls beyond the sixth are not executed");
        assert_eq!(starts, 6, "the first six calls are executed");
        drop(agent_loop);
        handle.abort();
    });
}
