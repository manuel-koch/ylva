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
use crate::agent::tools::Tool;

fn result_event(id: Uuid, name: &str, result: String, complete: bool) -> AgentEvent {
    AgentEvent::ToolDone {
        id,
        name: name.into(),
        result: result.clone(),
        requires_complete_delivery: complete,
        display_result: Some(result),
        result_metadata: Some(serde_json::json!({"source": "/skill/SKILL.md"})),
        result_metadata_schema: None,
        summary: vec![],
        duration: Duration::ZERO,
        content_sections: vec![],
        attachments: vec![],
    }
}

fn delivery_run_state() -> (RunState, mpsc::Receiver<UiUpdate>) {
    let (mut state, updates) = run_state();
    state.engine = Arc::new(crate::EngineImpl::new(
        Arc::new(crate::MockProvider::new(vec![], Duration::ZERO)),
        crate::EngineConfig {
            context_length: Some(4_000),
            ..state.engine.config()
        },
    ));
    state.own_run_id = Some(Uuid::new_v4());
    (state, updates)
}

#[test]
fn complete_delivery_rechecks_live_allowance_and_queued_sharing() {
    crate::test_support::run_locked(async {
        let (mut state, _updates) = delivery_run_state();
        let id = Uuid::new_v4();
        let allowance = tool_output_token_limit(
            "load_skill",
            &state.context,
            &[],
            state.engine.effective_context_length(),
            &[],
        )
        .unwrap();
        let text = " instructions".repeat(allowance / 2 + 20);
        assert!(matches!(
            state.enforce_complete_delivery(result_event(id, "load_skill", text.clone(), true)),
            AgentEvent::ToolDone { .. }
        ));
        let tool = tools::read::ReadTool::new(crate::SandboxContext::new(Arc::new(
            std::sync::RwLock::new(crate::Sandbox::new()),
        )));
        state.pending_tools.push_back(PendingTool {
            call: tool.create_call(Uuid::new_v4(), serde_json::json!({"path":"unused"})),
        });
        assert!(
            matches!(state.enforce_complete_delivery(result_event(id, "load_skill", text.clone(), true)), AgentEvent::ToolError { id: error_id, .. } if id == error_id)
        );
        state.pending_tools.clear();
        state
            .context
            .push_async_harness_notice(" live context change".repeat(allowance));
        assert!(matches!(
            state.enforce_complete_delivery(result_event(id, "load_skill", text.clone(), true)),
            AgentEvent::ToolError { .. }
        ));
        assert!(matches!(
            state.enforce_complete_delivery(result_event(id, "read", text.clone(), false)),
            AgentEvent::ToolDone { .. }
        ));
        assert!(matches!(
            state.enforce_complete_delivery(result_event(id, "agent_finish", text, true)),
            AgentEvent::ToolDone { .. }
        ));
    });
}

#[test]
fn complete_delivery_rejection_persists_one_error_and_never_success() {
    crate::test_support::run_locked(async {
        let (mut state, mut updates) = delivery_run_state();
        state.max_tool_iterations = 0;
        let directory = tempfile::tempdir().unwrap();
        let store = crate::Store::create(
            directory.path(),
            directory.path(),
            Uuid::new_v4(),
            "mock",
            "model",
            None,
        )
        .unwrap();
        state.context.set_store(store.clone());
        let id = Uuid::new_v4();
        state.context.push_assistant_tool_call(
            String::new(),
            AssistantStatus::Complete,
            vec![ToolCallRequest {
                id,
                name: "load_skill".into(),
                arguments: serde_json::json!({"name":"example"}),
                persisted_title_arguments: vec![],
            }],
        );
        state
            .handle_agent_event(
                1,
                result_event(id, "load_skill", " too large".repeat(4_000), true),
            )
            .await;
        assert_eq!(state.context.messages().iter().filter(|message| matches!(message, Message::ToolError { id: error_id, error, .. } if *error_id == id && error.contains("complete result exceeds"))).count(), 1);
        assert!(!state
            .context
            .messages()
            .iter()
            .any(|message| matches!(message, Message::ToolResult { .. })));
        assert!(
            matches!(updates.recv().await, Some(UiUpdate::ToolError { id: error_id, .. }) if id == error_id)
        );
        // A title command is a writer barrier after the queued transcript events.
        store.set_title("barrier".into()).await.unwrap();
        let replayed = crate::Store::read_messages(store.path()).unwrap();
        assert_eq!(replayed.iter().filter(|message| matches!(message, Message::ToolError { id: error_id, .. } if *error_id == id)).count(), 1);
        assert!(!replayed
            .iter()
            .any(|message| matches!(message, Message::ToolResult { .. })));
    });
}

#[test]
fn complete_delivery_success_is_unprojected_ordinary_results_remain_bounded_and_stale_results_drop()
{
    crate::test_support::run_locked(async {
        let (mut state, _updates) = delivery_run_state();
        state.max_tool_iterations = 0;
        let id = Uuid::new_v4();
        state
            .handle_agent_event(0, result_event(id, "load_skill", "stale".into(), true))
            .await;
        assert!(!state
            .context
            .messages()
            .iter()
            .any(|message| matches!(message, Message::ToolResult { .. })));
        state
            .handle_agent_event(
                1,
                result_event(id, "load_skill", "Complete body".into(), true),
            )
            .await;
        assert_eq!(
            state.context.messages().last(),
            state.context.model_messages().last()
        );
        let id = Uuid::new_v4();
        state
            .handle_agent_event(
                1,
                result_event(id, "read", " ordinary output".repeat(4_000), false),
            )
            .await;
        let model = state.context.model_messages();
        let canonical_result = state
            .context
            .messages()
            .iter()
            .find(|message| matches!(message, Message::ToolResult { value } if value.id == id))
            .unwrap();
        let projected_result = model
            .iter()
            .find(|message| matches!(message, Message::ToolResult { value } if value.id == id))
            .unwrap();
        assert!(
            canonical_result != projected_result,
            "ordinary result must retain its bounded projection"
        );
        assert!(
            matches!(canonical_result, Message::ToolResult { value } if value.result.len() > 50_000)
        );
    });
}

fn run_state() -> (RunState, mpsc::Receiver<UiUpdate>) {
    let (ui_tx, ui_rx) = mpsc::channel(100);
    let (agent_tx, _agent_rx) = mpsc::channel(100);
    let (compaction, _compaction_rx) = compaction::CompactionRuntime::new(
        None,
        compaction::COMPACTION_SUMMARY_PROMPT.to_string(),
        Duration::from_secs(compaction::DEFAULT_COMPACTION_TIMEOUT_SECS),
    );
    (
        RunState {
            engine: crate::agent::tests::test_engine(),
            registry: Arc::new(ToolRegistry::new()),
            ui_tx,
            agent_tx,
            parent_cancel: None,
            system_prompt: None,
            own_run_id: None,
            subagent_manager: None,
            max_tool_iterations: DEFAULT_MAX_TOOL_USE_ITERATIONS,
            batch_cap: MAX_EXECUTED_TOOL_CALLS_PER_BATCH,
            agent_type: None,
            session_id: None,
            current_store: Arc::new(Mutex::new(None)),
            store_for_commands: None,
            spawn_content_sections: HashMap::new(),
            is_subagent: true,
            context: Context::new(),
            compaction,
            startup_checks_finished: false,
            request_waiting_for_startup_checks: false,
            model_change_capacity_check: None,
            mode: AgentMode::Idle,
            current_cancel: None,
            current_tool_cancel: None,
            pending_steers: VecDeque::new(),
            pending_tools: VecDeque::new(),
            iterations: 0,
            clarify_started_at: None,
            generation: 1,
            cancelled_run_ids: std::collections::HashSet::new(),
            pending_subagent_finish_reprompt: false,
            iteration_cap_exceeded: None,
            stream_error: None,
            session_switch_frozen: false,
            execution_span: tracing::Span::none(),
        },
        ui_rx,
    )
}

#[test]
fn ignored_inputs_poll_again_instead_of_completing_an_idle_subagent() {
    crate::test_support::run_locked(async {
        let (mut state, mut updates) = run_state();
        let transcript = state.context.messages().to_vec();
        assert_eq!(
            state
                .handle_command(Some(AgentCommand::Prompt(String::new())))
                .await,
            LoopControl::PollAgain
        );
        assert_eq!(
            state
                .handle_agent_event(
                    0,
                    AgentEvent::StreamDone {
                        partial: "stale reply".to_string(),
                    },
                )
                .await,
            LoopControl::PollAgain
        );
        let run_id = Uuid::new_v4();
        state.cancelled_run_ids.insert(run_id);
        assert_eq!(
            state
                .handle_subagent_finish(crate::subagent_manager::FinishResult {
                    run_id,
                    call_id: Uuid::new_v4(),
                    origin: crate::subagent_manager::FinishResultOrigin::Runtime,
                    status: "success".to_string(),
                    result: Some("stale subagent result".to_string()),
                    error: None,
                    sources: vec![],
                })
                .await,
            LoopControl::PollAgain
        );
        assert_eq!(state.mode, AgentMode::Idle);
        assert_eq!(state.context.messages(), transcript);
        assert!(updates.try_recv().is_err());
        assert!(!state.cancelled_run_ids.contains(&run_id));
    });
}

#[test]
fn terminal_and_normal_handlers_preserve_distinct_completion_outcomes() {
    crate::test_support::run_locked(async {
        let (mut state, mut updates) = run_state();
        assert_eq!(
            state
                .handle_command(Some(AgentCommand::DenyTool {
                    id: Uuid::new_v4(),
                    reason: String::new(),
                }))
                .await,
            LoopControl::CheckCompletion
        );
        assert_eq!(
            state
                .handle_agent_event(
                    1,
                    AgentEvent::StreamDone {
                        partial: "committed answer".to_string(),
                    },
                )
                .await,
            LoopControl::CheckCompletion
        );
        assert!(matches!(
            state.context.messages().last(),
            Some(Message::Assistant {
                content,
                status: AssistantStatus::Complete,
                ..
            }) if content == "committed answer"
        ));
        assert_eq!(
            updates.try_recv().expect("completion update"),
            UiUpdate::StreamDone
        );
        assert_eq!(state.handle_command(None).await, LoopControl::Finished);
    });
}

#[test]
fn session_replacement_barrier_is_idempotent_blocks_mutations_and_requires_shutdown_freeze() {
    crate::test_support::run_locked(async {
        let (mut state, mut updates) = run_state();
        state.is_subagent = false;
        state.startup_checks_finished = true;
        assert_eq!(
            state.handle_command(Some(AgentCommand::Shutdown)).await,
            LoopControl::PollAgain
        );
        for _ in 0..2 {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            assert_eq!(
                state
                    .handle_command(Some(AgentCommand::PrepareSessionSwitch {
                        reply: sender.into(),
                    }))
                    .await,
                LoopControl::PollAgain
            );
            assert!(receiver.await.expect("accepted barrier"));
        }
        let view = state.context.model_messages();
        let generation = state.generation;
        for command in [
            AgentCommand::Prompt("blocked prompt".to_string()),
            AgentCommand::ClearContext,
            AgentCommand::Compact,
            AgentCommand::RecordModelChanged {
                provider_connection: "blocked".to_string(),
                model: "blocked".to_string(),
            },
        ] {
            assert_eq!(
                state.handle_command(Some(command)).await,
                LoopControl::PollAgain
            );
            assert!(matches!(
                updates.recv().await,
                Some(UiUpdate::Notice { run_id: None, text })
                    if text.contains("not accepted")
            ));
        }
        assert_eq!(state.context.model_messages(), view);
        assert_eq!(state.generation, generation);
        assert!(state.pending_steers.is_empty());
        assert!(state.model_change_capacity_check.is_none());
        assert_eq!(
            state
                .handle_agent_event(
                    state.generation,
                    AgentEvent::StreamDone {
                        partial: "late event".to_string(),
                    },
                )
                .await,
            LoopControl::PollAgain
        );
        assert_eq!(
            state
                .handle_subagent_finish(crate::subagent_manager::FinishResult {
                    run_id: Uuid::new_v4(),
                    call_id: Uuid::new_v4(),
                    origin: crate::subagent_manager::FinishResultOrigin::Runtime,
                    status: "success".to_string(),
                    result: Some("late subagent".to_string()),
                    error: None,
                    sources: vec![],
                })
                .await,
            LoopControl::PollAgain
        );
        assert_eq!(state.context.model_messages(), view);
        state
            .handle_command(Some(AgentCommand::ApproveTool { id: Uuid::new_v4() }))
            .await;
        state
            .handle_command(Some(AgentCommand::Stop {
                reason: "still allowed".to_string(),
            }))
            .await;
        assert!(state.session_switch_frozen);
        assert_eq!(
            state
                .handle_command(Some(AgentCommand::CancelSessionSwitch))
                .await,
            LoopControl::PollAgain
        );
        assert!(!state.session_switch_frozen);
        state
            .handle_command(Some(AgentCommand::RecordModelChanged {
                provider_connection: "allowed".to_string(),
                model: "allowed".to_string(),
            }))
            .await;
        assert!(state.model_change_capacity_check.is_some());
        state.model_change_capacity_check = None;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        state
            .handle_command(Some(AgentCommand::PrepareSessionSwitch {
                reply: sender.into(),
            }))
            .await;
        assert!(receiver.await.unwrap());
        assert_eq!(
            state.handle_command(Some(AgentCommand::Shutdown)).await,
            LoopControl::Finished
        );
    });
}

#[test]
fn session_replacement_barrier_checks_authoritative_pending_runtime_work() {
    crate::test_support::run_locked(async {
        let (mut state, _updates) = run_state();
        state.is_subagent = false;
        assert!(state.session_switch_quiescent());
        for mode in [
            AgentMode::Pending,
            AgentMode::Streaming,
            AgentMode::ToolPending,
            AgentMode::ToolRunning,
            AgentMode::ClarifyPending,
            AgentMode::Aborting,
        ] {
            state.mode = mode;
            assert!(!state.session_switch_quiescent());
            let (sender, receiver) = tokio::sync::oneshot::channel();
            state
                .handle_command(Some(AgentCommand::PrepareSessionSwitch {
                    reply: sender.into(),
                }))
                .await;
            assert!(!receiver.await.unwrap());
            assert!(!state.session_switch_frozen);
        }
        state.mode = AgentMode::Idle;
        state.pending_steers.push_back("queued steer".to_string());
        assert!(!state.session_switch_quiescent());
        state.pending_steers.clear();
        state.current_cancel = Some(CancellationToken::new());
        assert!(!state.session_switch_quiescent());
        state.current_cancel = None;
        state.current_tool_cancel = Some(CancellationToken::new());
        assert!(!state.session_switch_quiescent());
        state.current_tool_cancel = None;
        state.cancelled_run_ids.insert(Uuid::new_v4());
        assert!(!state.session_switch_quiescent());
        state.cancelled_run_ids.clear();
        state.pending_subagent_finish_reprompt = true;
        assert!(!state.session_switch_quiescent());
        state.pending_subagent_finish_reprompt = false;
        state.request_waiting_for_startup_checks = true;
        assert!(!state.session_switch_quiescent());
        state.request_waiting_for_startup_checks = false;
        state.model_change_capacity_check = Some(Box::pin(async { None }));
        assert!(!state.session_switch_quiescent());
        state.model_change_capacity_check = None;
        assert!(state.session_switch_quiescent());
        state.is_subagent = true;
        assert!(!state.session_switch_quiescent());
    });
}

#[test]
fn manual_command_noop_unavailable_and_subagent_notices_do_not_commit_messages() {
    crate::test_support::run_locked(async {
        let (mut state, mut updates) = run_state();
        let transcript = state.context.messages().to_vec();
        state.handle_command(Some(AgentCommand::Compact)).await;
        assert!(matches!(
            updates.recv().await,
            Some(UiUpdate::Notice { text, .. }) if text.contains("only in the root")
        ));
        state.is_subagent = false;
        state.handle_command(Some(AgentCommand::Compact)).await;
        assert!(matches!(
            updates.recv().await,
            Some(UiUpdate::Notice { text, run_id: None }) if text.contains("unavailable")
        ));
        let (compaction, _outcomes) = compaction::CompactionRuntime::new(
            Some(state.engine.clone()),
            "prompt".to_string(),
            Duration::from_secs(1),
        );
        state.compaction = compaction;
        state.handle_command(Some(AgentCommand::Compact)).await;
        assert!(matches!(
            updates.recv().await,
            Some(UiUpdate::Notice { text, run_id: None }) if text.contains("Nothing eligible")
        ));
        assert_eq!(state.context.messages(), transcript);
        assert!(state.pending_steers.is_empty());
    });
}

#[test]
fn session_replacement_barrier_counts_tool_subagent_and_summary_queues() {
    crate::test_support::run_locked(async {
        let (mut state, _updates) = run_state();
        state.is_subagent = false;
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(tools::clarify::ClarifyTool));
        state.pending_tools.push_back(PendingTool {
            call: registry
                .create_call(
                    "clarify",
                    Uuid::new_v4(),
                    serde_json::json!({"question": "Pick", "choices": ["a", "b"]}),
                )
                .expect("clarify"),
        });
        assert!(!state.session_switch_quiescent());
        state.pending_tools.clear();
        let (subagent_updates, _subagent_receiver) = mpsc::unbounded_channel();
        let mut manager = crate::subagent_manager::SubagentManager::new(
            subagent_updates,
            Box::new(|_, _, _| crate::subagent_manager::SpawnOutcome {
                command_sender: None,
                handle: tokio::spawn(async {}),
            }),
            Some(1),
        );
        manager.spawn(
            "first task".to_string(),
            None,
            "default".to_string(),
            "first".to_string(),
            Uuid::new_v4(),
        );
        manager.spawn(
            "queued task".to_string(),
            None,
            "default".to_string(),
            "queued".to_string(),
            Uuid::new_v4(),
        );
        assert_eq!(manager.outstanding_count(), 2);
        state.subagent_manager = Some(manager);
        assert!(!state.session_switch_quiescent());
        state.subagent_manager.as_mut().unwrap().cancel_all().await;
        assert!(state.session_switch_quiescent());

        let summary_engine: Arc<dyn Engine> = Arc::new(crate::llm::EngineImpl::new(
            Arc::new(crate::llm::MockProvider::new(
                vec!["summary".to_string()],
                Duration::ZERO,
            )),
            state.engine.config(),
        ));
        let (compaction, mut outcomes) = compaction::CompactionRuntime::new(
            Some(summary_engine),
            "prompt".to_string(),
            Duration::from_secs(1),
        );
        state.compaction = compaction;
        state.context.push_user("previous task".to_string());
        state
            .context
            .push_assistant("previous answer".to_string(), AssistantStatus::Complete);
        state.context.push_user("latest task".to_string());
        state.compaction.schedule_manual_summary(
            &state.context,
            state.engine.as_ref(),
            state.generation,
        );
        assert!(!state.session_switch_quiescent());
        let outcome = outcomes.recv().await.expect("summary outcome");
        state.compaction.on_outcome(outcome, state.generation);
        assert!(!state.session_switch_quiescent());
        state.compaction.reset().await;
        assert!(state.session_switch_quiescent());
        state.session_switch_frozen = true;
        state.pending_subagent_finish_reprompt = true;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        state
            .handle_command(Some(AgentCommand::PrepareSessionSwitch {
                reply: sender.into(),
            }))
            .await;
        assert!(!receiver.await.unwrap());
        assert!(state.session_switch_frozen);
        assert_eq!(
            state.handle_command(Some(AgentCommand::Shutdown)).await,
            LoopControl::PollAgain
        );
    });
}

#[test]
fn stop_returns_only_after_every_cancelled_subagent_task_has_finished() {
    crate::test_support::run_locked(async {
        let (mut state, _updates) = run_state();
        state.is_subagent = false;

        let finished_flags: Arc<Mutex<Vec<Arc<std::sync::atomic::AtomicBool>>>> =
            Arc::new(Mutex::new(Vec::new()));
        let recorded_flags = Arc::clone(&finished_flags);
        let spawn_fn: crate::subagent_manager::SpawnFn =
            Box::new(move |_request, cancellation_token, _subagent_ui_tx| {
                let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
                recorded_flags.lock().unwrap().push(Arc::clone(&finished));
                let handle = tokio::spawn(async move {
                    cancellation_token.cancelled().await;
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    finished.store(true, std::sync::atomic::Ordering::SeqCst);
                });
                crate::subagent_manager::SpawnOutcome {
                    command_sender: None,
                    handle,
                }
            });
        let (subagent_ui_tx, _subagent_ui_rx) = mpsc::unbounded_channel();
        let mut manager =
            crate::subagent_manager::SubagentManager::new(subagent_ui_tx, spawn_fn, Some(2));
        for index in 0..2 {
            manager.spawn(
                format!("task {index}"),
                None,
                "default".to_string(),
                format!("task {index}"),
                Uuid::new_v4(),
            );
        }
        state.subagent_manager = Some(manager);

        state
            .handle_command(Some(AgentCommand::Stop {
                reason: "test stop".to_string(),
            }))
            .await;

        let flags = finished_flags.lock().unwrap();
        assert_eq!(flags.len(), 2);
        for finished in flags.iter() {
            assert!(
                finished.load(std::sync::atomic::Ordering::SeqCst),
                "a cancelled subagent task was still running when Stop returned"
            );
        }
    });
}

#[test]
fn stop_cancels_two_running_subagents_and_one_queued_subagent() {
    crate::test_support::run_locked(async {
        let (mut state, mut updates) = run_state();
        state.is_subagent = false;

        let cancelled_tokens = Arc::new(Mutex::new(Vec::new()));
        let recorded_tokens = Arc::clone(&cancelled_tokens);
        let spawn_fn: crate::subagent_manager::SpawnFn =
            Box::new(move |_request, cancellation_token, _subagent_ui_tx| {
                recorded_tokens.lock().unwrap().push(cancellation_token);
                crate::subagent_manager::SpawnOutcome {
                    command_sender: None,
                    handle: tokio::spawn(async {}),
                }
            });
        let (subagent_ui_tx, _subagent_ui_rx) = mpsc::unbounded_channel();
        let mut manager =
            crate::subagent_manager::SubagentManager::new(subagent_ui_tx, spawn_fn, Some(2));
        let call_ids = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
        let run_ids: Vec<Uuid> = call_ids
            .iter()
            .enumerate()
            .map(|(index, call_id)| {
                manager
                    .spawn(
                        format!("task {index}"),
                        None,
                        "default".to_string(),
                        format!("task {index}"),
                        *call_id,
                    )
                    .run_id()
            })
            .collect();
        assert_eq!(manager.outstanding_count(), 3);
        assert_eq!(manager.queue_len(), 1);
        state.subagent_manager = Some(manager);

        state
            .handle_command(Some(AgentCommand::Stop {
                reason: "test stop".to_string(),
            }))
            .await;

        let mut cancelled_tool_done_ids = Vec::new();
        let mut cancelled_subagent_done_run_ids = Vec::new();
        while let Ok(update) = updates.try_recv() {
            match update {
                UiUpdate::ToolDone { id, summary, .. } => {
                    if summary
                        .iter()
                        .any(|(key, value)| key == "status" && value == "cancelled")
                    {
                        cancelled_tool_done_ids.push(id);
                    }
                }
                UiUpdate::SubagentDone { run_id, status, .. } if status == "cancelled" => {
                    cancelled_subagent_done_run_ids.push(run_id);
                }
                _ => {}
            }
        }
        assert_eq!(cancelled_tool_done_ids.len(), 3);
        for call_id in call_ids {
            assert!(cancelled_tool_done_ids.contains(&call_id));
        }
        let manager = state.subagent_manager.as_ref().unwrap();
        assert_eq!(manager.outstanding_count(), 0);
        assert_eq!(manager.queue_len(), 0);
        for run_id in run_ids {
            assert!(state.cancelled_run_ids.contains(&run_id));
        }
        assert_eq!(cancelled_subagent_done_run_ids.len(), 3);
        for run_id in state.cancelled_run_ids.iter() {
            assert!(cancelled_subagent_done_run_ids.contains(run_id));
        }
        assert!(cancelled_tokens
            .lock()
            .unwrap()
            .iter()
            .all(CancellationToken::is_cancelled));
    });
}

#[test]
fn clear_context_cancels_every_running_and_queued_subagent() {
    crate::test_support::run_locked(async {
        let (mut state, _updates) = run_state();
        state.is_subagent = false;

        let cancelled_tokens = Arc::new(Mutex::new(Vec::new()));
        let recorded_tokens = Arc::clone(&cancelled_tokens);
        let spawn_fn: crate::subagent_manager::SpawnFn =
            Box::new(move |_request, cancellation_token, _subagent_ui_tx| {
                recorded_tokens.lock().unwrap().push(cancellation_token);
                crate::subagent_manager::SpawnOutcome {
                    command_sender: None,
                    handle: tokio::spawn(async {}),
                }
            });
        let (subagent_ui_tx, _subagent_ui_rx) = mpsc::unbounded_channel();
        let mut manager =
            crate::subagent_manager::SubagentManager::new(subagent_ui_tx, spawn_fn, Some(2));
        let run_ids: Vec<Uuid> = (0..3)
            .map(|index| {
                manager
                    .spawn(
                        format!("task {index}"),
                        None,
                        "default".to_string(),
                        format!("task {index}"),
                        Uuid::new_v4(),
                    )
                    .run_id()
            })
            .collect();
        state.subagent_manager = Some(manager);
        state.context.push_user("pre-clear content".to_string());

        state.handle_command(Some(AgentCommand::ClearContext)).await;

        let manager = state.subagent_manager.as_ref().unwrap();
        assert_eq!(manager.outstanding_count(), 0);
        assert_eq!(manager.queue_len(), 0);
        assert!(!state.context.messages().iter().any(|message| {
            matches!(message, crate::agent::Message::User { content, .. } if content == "pre-clear content")
        }));
        for run_id in run_ids {
            assert!(state.cancelled_run_ids.contains(&run_id));
        }
        assert!(cancelled_tokens
            .lock()
            .unwrap()
            .iter()
            .all(CancellationToken::is_cancelled));
    });
}

#[test]
fn busy_manual_command_snapshots_immediately_excludes_queued_steers_and_stop_cancels_it() {
    crate::test_support::run_locked(async {
        let (mut state, mut updates) = run_state();
        state.is_subagent = false;
        let (compaction, _outcomes) = compaction::CompactionRuntime::new(
            Some(state.engine.clone()),
            "prompt".to_string(),
            Duration::from_secs(1),
        );
        state.compaction = compaction;
        state.context.push_user("previous task".to_string());
        state
            .context
            .push_assistant("previous answer".to_string(), AssistantStatus::Complete);
        state.context.push_user("latest task".to_string());
        state.mode = AgentMode::Streaming;
        state.current_cancel = Some(CancellationToken::new());
        state.pending_steers.push_back("not committed".to_string());
        let transcript = state.context.messages().to_vec();
        state.handle_command(Some(AgentCommand::Compact)).await;
        assert!(state.compaction.summary_pending());
        assert_eq!(state.mode, AgentMode::Streaming);
        assert!(!state.current_cancel.as_ref().unwrap().is_cancelled());
        assert_eq!(state.pending_steers.len(), 1);
        state.handle_command(Some(AgentCommand::Compact)).await;
        assert_eq!(state.context.messages(), transcript);
        state
            .handle_command(Some(AgentCommand::Stop {
                reason: "cancel".to_string(),
            }))
            .await;
        assert!(!state.compaction.summary_pending());
        let mut notices = Vec::new();
        while let Ok(update) = updates.try_recv() {
            if let UiUpdate::Notice { text, run_id } = update {
                assert_eq!(run_id, None);
                notices.push(text);
            }
        }
        assert_eq!(notices.len(), 3);
        assert_eq!(notices[0], "Started manual context compaction.");
        assert_eq!(
            notices[1],
            "Already compacting; no additional summary was queued."
        );
        assert_eq!(
            notices[2],
            "Manual context compaction cancelled; the model view is unchanged."
        );
    });
}

#[test]
fn busy_manual_summary_waits_for_a_completed_turn_and_releases_the_stream_barrier() {
    crate::test_support::run_locked(async {
        let (mut state, _updates) = run_state();
        state.is_subagent = false;
        let summary_engine: Arc<dyn Engine> = Arc::new(crate::llm::EngineImpl::new(
            Arc::new(crate::llm::MockProvider::new(
                vec!["short summary".to_string()],
                Duration::ZERO,
            )),
            state.engine.config(),
        ));
        let (compaction, mut outcomes) = compaction::CompactionRuntime::new(
            Some(summary_engine),
            "prompt".to_string(),
            Duration::from_secs(1),
        );
        state.compaction = compaction;
        state.context.push_user("previous task ".repeat(500));
        state
            .context
            .push_assistant("previous answer ".repeat(500), AssistantStatus::Complete);
        let boundary = state.context.messages().len();
        state.context.push_user("latest task".to_string());
        state.mode = AgentMode::Streaming;
        state.current_cancel = Some(CancellationToken::new());
        state.handle_command(Some(AgentCommand::Compact)).await;
        state
            .compaction
            .on_outcome(outcomes.recv().await.unwrap(), state.generation);
        assert!(state.context.compaction().is_none());
        assert!(!state.session_switch_quiescent());
        state
            .handle_agent_event(
                state.generation,
                AgentEvent::StreamDone {
                    partial: "latest completed answer".to_string(),
                },
            )
            .await;
        assert_eq!(state.context.compaction().unwrap().boundary, boundary);
        assert!(state.current_cancel.is_none());
        assert!(state.session_switch_quiescent());
        assert_eq!(
            &state.context.model_messages()[state.context.leading_system_len() + 1..],
            &state.context.messages()[boundary..],
        );
    });
}
