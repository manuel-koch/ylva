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
fn oversized_subagent_result_preserves_canonical_output_and_bounds_parent_projection() {
    crate::test_support::run_locked(async {
        use crate::agent::tools::{default_registry, ToolRegistryConfig};
        use crate::subagent_manager::{FinishResult, SubagentManager};

        let root_engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
            Arc::new(MockProvider::with_script(
                vec![MockEvent::Text("I will combine the results.".to_string())],
                Duration::ZERO,
            )),
            EngineConfig {
                provider_connection: "mock".to_string(),
                model: "root-model".to_string(),
                temperature: None,
                context_length: Some(20_000),
                api_base_url: None,
                api_key: None,
                reasoning: None,
                compact_target_ratio: 0.9,
                soft_compaction_threshold: 0.95,
                hard_compaction_threshold: 0.99,
            },
        ));
        let recorded_summaries = Arc::new(Mutex::new(Vec::new()));
        let summary_engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
            Arc::new(ScriptedSummaryProvider {
                recorded: Arc::clone(&recorded_summaries),
                fail: false,
            }),
            EngineConfig {
                provider_connection: "mock".to_string(),
                model: "summary-model".to_string(),
                temperature: None,
                context_length: Some(16_000),
                api_base_url: None,
                api_key: None,
                reasoning: None,
                compact_target_ratio: 0.5,
                soft_compaction_threshold: 0.7,
                hard_compaction_threshold: 0.9,
            },
        ));
        let (subagent_ui_tx, _subagent_ui_rx) = mpsc::unbounded_channel();
        let manager = SubagentManager::new(
            subagent_ui_tx,
            Box::new(
                |_request, _cancel, _ui_tx| crate::subagent_manager::SpawnOutcome {
                    command_sender: None,
                    handle: tokio::spawn(async {}),
                },
            ),
            Some(1),
        );
        let (finish_tx, finish_rx) = mpsc::unbounded_channel();
        let (_parent_subagent_ui_tx, parent_subagent_ui_rx) = mpsc::unbounded_channel();
        let (agent_loop, _ui_rx) = AgentLoop::new(
            root_engine,
            Arc::new(
                default_registry(test_sandbox(), ToolRegistryConfig::default())
                    .expect("valid sandbox fixture"),
            ),
        );
        let mut agent_loop = agent_loop
            .with_subagent_manager(manager, finish_rx, parent_subagent_ui_rx)
            .with_compaction_engine(summary_engine);
        let handle = agent_loop.run();
        agent_loop
            .send_command(AgentCommand::Prompt(
                "Use the subagent findings.".to_string(),
            ))
            .await;

        tokio::time::sleep(Duration::from_millis(20)).await;
        finish_tx
            .send(FinishResult {
                run_id: Uuid::new_v4(),
                call_id: Uuid::new_v4(),
                origin: crate::subagent_manager::FinishResultOrigin::Agent,
                status: "success".to_string(),
                result: Some("verbose subagent report ".repeat(6_000)),
                error: None,
                sources: vec!["src/store.rs".to_string()],
            })
            .expect("parent completion receiver remains open");
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(agent_loop);
        let context = tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("root loop stops")
            .expect("root task joins")
            .expect("root loop succeeds");

        let completion_notice = context.messages().iter().find_map(|message| match message {
            Message::User {
                content,
                kind: UserMessageKind::ImplicitToolNote,
            } if content.contains("Subagent completion event") => Some(content),
            _ => None,
        });
        let completion_notice = completion_notice.expect("completion event reaches parent");
        assert!(completion_notice.contains("verbose subagent report"));
        let projected_notice = context
            .model_messages()
            .into_iter()
            .find_map(|message| match message {
                Message::User {
                    content,
                    kind: UserMessageKind::ImplicitToolNote,
                } if content.contains("Subagent completion event") => Some(content),
                _ => None,
            })
            .expect("projected subagent event");
        let event: Value = serde_json::from_str(
            projected_notice
                .lines()
                .find(|line| line.starts_with('{'))
                .expect("structured event"),
        )
        .expect("valid completion JSON");
        assert!(event["result"]
            .as_str()
            .expect("result")
            .contains(compaction::recovery::RESULT_OMISSION_MARKER));
        assert_eq!(event["result_reduced"], true);
        assert!(projected_notice.len() < completion_notice.len());
        for notice in [completion_notice, &projected_notice] {
            assert!(notice.contains("self-contained answer"));
            assert!(notice.contains("Preserve the user's requested format"));
            assert!(notice.contains(
                "Do not assume tool results or subagent responses are visible to the user"
            ));
            assert!(!notice.contains("bubbles"));
            assert!(!notice.contains("do not repeat or quote it"));
        }
        assert!(
            recorded_summaries
                .lock()
                .expect("summary requests")
                .is_empty(),
            "report reduction does not block the loop on an extra model request"
        );
    });
}

#[test]
fn subagent_role_run_returns_immediately_after_finish_tool_is_called() {
    crate::test_support::run_locked(async {
        // Regression/integration test for Phase 6.9: a `Role::Subagent` agent
        // whose model calls `agent_finish` must have its context carry the tool
        // result, must send the `FinishResult` through `finish_tx`, and must
        // return from `run()` without waiting for a tool-free `StreamDone`.
        use crate::agent::tools::{
            agent_finish::AgentFinishToolConfig, default_registry, ToolRegistryConfig,
        };

        let script = vec![MockEvent::Delta(DeltaChunk {
            tool_call_index: 0,
            tool_call_id: Some("call_1".into()),
            function_name: Some(tools::agent_finish::AgentFinishTool::NAME.to_string()),
            arguments_fragment: Some("{\"status\":\"success\",\"result\":\"done\"}".to_string()),
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

        let (finish_tx, mut finish_rx) = mpsc::channel(1);
        let (subagent_ui_tx, _subagent_ui_rx) = mpsc::unbounded_channel();
        let run_id = Uuid::new_v4();
        let call_id = Uuid::new_v4();

        let registry = Arc::new(
            default_registry(
                test_sandbox(),
                ToolRegistryConfig {
                    allowed_tools: vec![],
                    agent_finish: Some(AgentFinishToolConfig {
                        finish_tx: finish_tx.clone(),
                        run_id,
                        call_id,
                    }),
                    ..Default::default()
                },
            )
            .expect("valid sandbox fixture"),
        );

        let (mut agent_loop, mut ui_rx) = AgentLoop::new(engine, registry);
        agent_loop = agent_loop.with_subagent_role(SubagentRoleConfig {
            run_id,
            call_id,
            session_id: None,
            briefing: Some("do the task".to_string()),
            parent_cancel: CancellationToken::new(),
            subagent_ui_tx,
            finish_tx,
        });

        let handle = agent_loop.run();
        agent_loop
            .send_command(AgentCommand::Prompt("subagent task".into()))
            .await;

        let context = tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("subagent run() should return promptly after finish")
            .expect("run() task should not panic")
            .expect("run() should not return an error");

        assert!(
            context.messages().iter().any(|message| matches!(
                message,
                Message::ToolResult {
                    value: ToolResult { name, .. },
                } if name == tools::agent_finish::AgentFinishTool::NAME
            )),
            "context should carry the finish tool's result"
        );
        assert!(
            context.messages().iter().any(|message| matches!(
                message,
                Message::System { content } if content.contains("tool-use") && content.contains(&DEFAULT_MAX_TOOL_USE_ITERATIONS.to_string())
            )),
            "a subagent must receive a heads-up system message stating its \
             tool-use iteration budget, so it can wrap up before hitting it"
        );

        let finish_result = tokio::time::timeout(Duration::from_secs(1), finish_rx.recv())
            .await
            .expect("finish_rx should receive promptly")
            .expect("finish_tx should not be dropped without sending");
        assert_eq!(finish_result.run_id, run_id);
        assert_eq!(finish_result.call_id, call_id);
        assert_eq!(finish_result.status, "success");
        assert_eq!(finish_result.result.as_deref(), Some("done"));

        // Drain the ui updates emitted along the way; we only assert that a
        // ToolDone for "agent_finish" was sent among them.
        let mut saw_finish_tool_done = false;
        while let Ok(Some(update)) =
            tokio::time::timeout(Duration::from_millis(50), ui_rx.recv()).await
        {
            if matches!(update, UiUpdate::ToolDone { ref name, .. } if name == tools::agent_finish::AgentFinishTool::NAME)
            {
                saw_finish_tool_done = true;
            }
        }
        assert!(
            saw_finish_tool_done,
            "expected a ToolDone update for finish"
        );
    });
}

#[test]
fn root_role_does_not_receive_the_subagent_iteration_budget_heads_up() {
    crate::test_support::run_locked(async {
        // The heads-up message is only meaningful for a `Role::Subagent` loop
        // (only it can be silently cut off with no one to explain things to);
        // a root loop's `Interrupted` assistant bubble already tells a human
        // what happened, so it must not get this extra system message too.
        let registry = Arc::new(
            tools::default_registry(test_sandbox(), tools::ToolRegistryConfig::default())
                .expect("valid sandbox fixture"),
        );
        let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
            Arc::new(MockProvider::new(
                vec!["answer".to_string()],
                Duration::from_millis(0),
            )),
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

        let (mut agent_loop, _ui_rx) = AgentLoop::new(engine, registry);
        let handle = agent_loop.run();
        agent_loop
            .send_command(AgentCommand::Prompt("hi".into()))
            .await;
        drop(agent_loop);

        let context = tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("root run() should return promptly after being dropped")
            .expect("run() task should not panic")
            .expect("run() should not return an error");

        assert!(
            !context
                .messages()
                .iter()
                .any(|message| matches!(message, Message::System { content } if content.contains("tool-use"))),
            "a root loop must not receive the subagent-only iteration-budget heads-up"
        );
    });
}

#[test]
fn subagent_role_iteration_cap_exceeded_returns_err_not_unstructured_ok() {
    crate::test_support::run_locked(async {
        // Regression test: a `Role::Subagent` loop whose model keeps calling
        // tools without ever calling `agent_finish` must, once the iteration
        // cap trips, report a clear `Err` (→ `drive_subagent_to_completion`'s
        // `Ok(Err(error))` arm, `status: "failure"`) rather than the
        // "unstructured" `Ok(context)` fallback that would otherwise mistake
        // the guard's boilerplate message for a genuine final answer.
        // The mock replays this single scripted tool call on every fresh
        // request, so the subagent keeps calling "read" (auto-approved inside
        // the sandbox root, so no approval round-trip is needed) forever
        // without ever calling `agent_finish`, tripping the iteration guard.
        let script = vec![MockEvent::Delta(DeltaChunk {
            tool_call_index: 0,
            tool_call_id: Some("call_1".into()),
            function_name: Some("read".to_string()),
            arguments_fragment: Some(
                serde_json::json!({
                    "path": "Cargo.toml",
                })
                .to_string(),
            ),
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

        let registry = Arc::new(
            tools::default_registry(test_sandbox(), tools::ToolRegistryConfig::default())
                .expect("valid sandbox fixture"),
        );

        let (finish_tx, _finish_rx) = mpsc::channel(1);
        let (subagent_ui_tx, mut subagent_ui_rx) = mpsc::unbounded_channel();
        let run_id = Uuid::new_v4();
        let call_id = Uuid::new_v4();

        let (mut agent_loop, mut ui_rx) = AgentLoop::new(engine, registry);
        agent_loop =
            agent_loop
                .with_max_tool_iterations(3)
                .with_subagent_role(SubagentRoleConfig {
                    run_id,
                    call_id,
                    session_id: None,
                    briefing: Some("do the task".to_string()),
                    parent_cancel: CancellationToken::new(),
                    subagent_ui_tx,
                    finish_tx,
                });

        let handle = agent_loop.run();
        agent_loop
            .send_command(AgentCommand::Prompt("subagent task".into()))
            .await;

        // Drain `ui_rx` *while* `run()` is in flight, exactly like the
        // TUI/CLI drivers do. The channel is bounded (capacity 100) and
        // every tool round emits several `UiUpdate`s, so an undrained
        // receiver fills it long before `DEFAULT_MAX_TOOL_USE_ITERATIONS` rounds
        // elapse — `ui_tx.send().await` then parks the whole loop, the
        // iteration cap never trips, and `run()` never returns.
        let mut handle = handle;
        let mut collected_updates: Vec<UiUpdate> = Vec::new();
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                tokio::select! {
                    biased;
                    joined = &mut handle => break joined,
                    update = ui_rx.recv() => match update {
                        Some(update) => collected_updates.push(update),
                        // The loop itself keeps a sender alive for the
                        // whole test, so the channel never closes here;
                        // park on `handle` instead of spinning.
                        None => std::future::pending::<()>().await,
                    },
                }
            }
        })
        .await
        .expect("subagent run() should return promptly once the iteration cap trips")
        .expect("run() task should not panic");

        // Collect anything `run()` posted just before returning that the
        // loop above had not polled yet (both branches were ready, and
        // `biased` preferred `handle`).
        while let Ok(update) = ui_rx.try_recv() {
            collected_updates.push(update);
        }

        let error = result.expect_err(
            "a subagent hitting the iteration cap must return Err, not Ok(context) \
             (which would be mistaken for an 'unstructured' final answer)",
        );
        assert!(
            error.to_string().contains("iterations"),
            "error should explain the iteration cap was hit; got: {error}"
        );

        // Require the error itself, not just correct fields if one happens to appear.
        let errors = collected_updates
            .iter()
            .filter_map(|update| match update {
                UiUpdate::Error { message, run_id } => Some((message, run_id)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            errors.len(),
            1,
            "the subagent must emit its iteration-cap error"
        );
        assert_eq!(errors[0].1, &Some(run_id));
        assert!(errors[0].0.contains("iterations"));
        assert_eq!(
            collected_updates
                .iter()
                .filter(|update| matches!(update, UiUpdate::StreamStarted))
                .count(),
            3,
            "the configured limit must allow exactly three requests"
        );
        assert_eq!(
            collected_updates
                .iter()
                .filter(|update| matches!(update, UiUpdate::ToolDone { .. }))
                .count(),
            3,
            "the configured limit must execute exactly three tool rounds"
        );
        // Drain the subagent ui channel too; just ensure nothing panics.
        while subagent_ui_rx.try_recv().is_ok() {}
    });
}

#[tokio::test]
async fn resolve_spawn_subagent_pushes_spawned_status_and_starts_subagent() {
    use crate::agent::tools::spawn_subagent::SpawnSubagentTool;
    use crate::agent::tools::Tool;
    use crate::subagent_manager::SubagentManager;
    use std::sync::Mutex;

    let call_id = Uuid::new_v4();
    let tool = SpawnSubagentTool::new(true);
    let call = tool.create_call(
        call_id,
        serde_json::json!({
            "label": "Do the thing",
            "task": "do the thing",
            "briefing": "context"
        }),
    );
    let pending = PendingTool { call };

    let started: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let started_clone = started.clone();
    let (subagent_ui_tx, mut subagent_ui_rx) = mpsc::unbounded_channel();
    let manager = SubagentManager::new(
        subagent_ui_tx,
        Box::new(move |request, _cancel, _ui_tx| {
            started_clone.lock().unwrap().push(request.task.clone());
            crate::subagent_manager::SpawnOutcome {
                command_sender: None,
                handle: tokio::spawn(async {}),
            }
        }),
        Some(1),
    );
    let mut subagent_manager = Some(manager);

    let (ui_tx, mut ui_rx) = mpsc::channel(8);
    let mut context = Context::new();
    let mut spawn_content_sections: HashMap<Uuid, Vec<crate::agent::tools::Section>> =
        HashMap::new();

    resolve_spawn_subagent(
        pending,
        &mut subagent_manager,
        &mut context,
        &ui_tx,
        &mut spawn_content_sections,
    )
    .await;

    assert_eq!(
        started.lock().unwrap().as_slice(),
        &["do the thing".to_string()],
        "the stub spawn_fn should have been invoked with the task"
    );
    assert!(
        subagent_manager.as_ref().unwrap().is_running(),
        "manager should report a running subagent"
    );

    let tool_result = context
        .messages()
        .iter()
        .find_map(|message| match message {
            Message::ToolResult {
                value: ToolResult {
                    id, name, result, ..
                },
            } if *id == call_id && name == "spawn_subagent" => Some(result.clone()),
            _ => None,
        })
        .expect("spawn_subagent tool result should be pushed into context");
    let parsed: serde_json::Value = serde_json::from_str(&tool_result).unwrap();
    assert_eq!(parsed["event"], "subagent_accepted");
    assert_eq!(parsed["tool_call_id"], call_id.to_string());
    assert_eq!(parsed["label"], "Do the thing");
    assert_eq!(parsed["status"], "running");
    assert_eq!(parsed["outstanding_count"], 1);
    assert_eq!(parsed["all_done"], false);

    // Only `ToolStart` comes back on the caller's own `ui_tx` — resolving
    // the spawn is not the call finishing, so no `ToolDone` is sent here.
    assert!(matches!(
        ui_rx.recv().await,
        Some(UiUpdate::ToolStart { name, .. }) if name == "spawn_subagent"
    ));

    // `SubagentManager::start` first reports `SubagentStarted` (the
    // subagent-lifecycle boundary for the drill-in view), then the
    // "running" status as a `ToolProgress` on the manager's own
    // `subagent_ui_tx` (forwarded to the parent's `ui_tx` in production
    // by `AgentLoop::run`'s `recv_subagent_ui` arm — bypassed here since
    // this test calls `resolve_spawn_subagent` directly).
    assert!(matches!(
        subagent_ui_rx.recv().await,
        Some(UiUpdate::SubagentStarted { agent_type, task, .. })
            if agent_type == "default" && task == "do the thing"
    ));
    match subagent_ui_rx.recv().await {
        Some(UiUpdate::ToolProgress {
            id,
            title_arguments,
        }) => {
            assert_eq!(id, call_id);
            let status = title_arguments
                .iter()
                .find(|(key, _)| key == "status")
                .map(|(_, value)| value.clone());
            assert_eq!(status, Some(serde_json::json!("running")));
        }
        other => panic!("expected a ToolProgress update, got: {other:?}"),
    }

    // The task/briefing content sections shown while pending approval
    // must be stashed so the eventual real `ToolDone` (sent once the
    // subagent actually finishes) can still show them alongside the result.
    let stashed = spawn_content_sections
        .get(&call_id)
        .expect("task/briefing sections should be stashed for this call id");
    let labels: Vec<Option<String>> = stashed
        .iter()
        .map(|section| match section {
            crate::agent::tools::Section::Content(content) => content.label.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(
        labels,
        vec![Some("Task".to_string()), Some("Briefing".to_string())]
    );
}
