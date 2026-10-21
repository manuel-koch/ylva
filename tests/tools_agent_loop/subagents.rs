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

struct ConcurrencyScenario {
    agent: AgentLoop,
    ui_rx: mpsc::Receiver<UiUpdate>,
    finish_tx: mpsc::UnboundedSender<ylva::subagent_manager::FinishResult>,
    spawned_requests: Arc<Mutex<Vec<ylva::subagent_manager::SpawnRequest>>>,
    subagent_ui_senders: Arc<Mutex<Vec<mpsc::UnboundedSender<UiUpdate>>>>,
    interval: Duration,
}

fn build_concurrency_scenario(
    max_concurrent_subagents: Option<usize>,
    subagent_count: usize,
) -> ConcurrencyScenario {
    use ylva::subagent_manager::{SpawnFn, SpawnRequest, SubagentManager};

    let registry = Arc::new(
        default_registry(
            make_sandbox(),
            ToolRegistryConfig {
                include_spawn_subagent: true,
                auto_approve: std::collections::HashSet::from([AutoApproveTool::Spawn]),
                ..ToolRegistryConfig::default()
            },
        )
        .expect("valid sandbox fixture"),
    );
    let interval = Duration::from_millis(5);
    let spawn_calls = (0..subagent_count)
        .map(|index| {
            call(
                "spawn_subagent",
                index,
                serde_json::json!({
                    "label": format!("Subagent {index}"),
                    "task": format!("task {index}"),
                }),
            )
        })
        .collect();
    let provider = SequencedMockProvider::new(
        vec![
            spawn_calls,
            vec![MockEvent::Text("working".to_string())],
            vec![MockEvent::Text("combined answer".to_string())],
        ],
        interval,
    );
    let (mut agent, ui_rx) = AgentLoop::new(
        Arc::new(EngineImpl::new(
            Arc::new(provider),
            common::test_engine_config(),
        )),
        Arc::clone(&registry),
    );

    let (finish_tx, finish_rx) = mpsc::unbounded_channel();
    let (subagent_ui_tx, subagent_ui_rx) = mpsc::unbounded_channel();
    let spawned_requests = Arc::new(Mutex::new(Vec::<SpawnRequest>::new()));
    let recorded_requests = Arc::clone(&spawned_requests);
    let subagent_ui_senders = Arc::new(Mutex::new(Vec::<mpsc::UnboundedSender<UiUpdate>>::new()));
    let recorded_subagent_ui_senders = Arc::clone(&subagent_ui_senders);
    let spawn_fn: SpawnFn = Box::new(move |request, _cancel, subagent_ui_tx| {
        recorded_requests
            .lock()
            .expect("record subagent spawn")
            .push(request);
        recorded_subagent_ui_senders
            .lock()
            .expect("record subagent UI sender")
            .push(subagent_ui_tx);
        ylva::subagent_manager::SpawnOutcome {
            command_sender: None,
            handle: tokio::spawn(async {}),
        }
    });
    let manager = SubagentManager::new(subagent_ui_tx, spawn_fn, max_concurrent_subagents);
    agent = agent.with_subagent_manager(manager, finish_rx, subagent_ui_rx);
    ConcurrencyScenario {
        agent,
        ui_rx,
        finish_tx,
        spawned_requests,
        subagent_ui_senders,
        interval,
    }
}

async fn collect_through_working_response(
    ui_rx: &mut mpsc::Receiver<UiUpdate>,
    interval: Duration,
) -> Vec<UiUpdate> {
    let mut saw_working_response = false;
    tokio::time::timeout(
        Duration::from_secs(5),
        collect_until_advancing(ui_rx, interval, |update| {
            if matches!(update, UiUpdate::AppendText(text) if text == "working") {
                saw_working_response = true;
            }
            saw_working_response && matches!(update, UiUpdate::StreamDone)
        }),
    )
    .await
    .expect("root should complete the intermediate response while subagents remain active")
}

async fn collect_final_root_response(
    ui_rx: &mut mpsc::Receiver<UiUpdate>,
    interval: Duration,
) -> Vec<UiUpdate> {
    let mut saw_combined_answer = false;
    tokio::time::timeout(
        Duration::from_secs(5),
        collect_until_advancing(ui_rx, interval, |update| {
            if matches!(update, UiUpdate::AppendText(text) if text == "combined answer") {
                saw_combined_answer = true;
            }
            saw_combined_answer && matches!(update, UiUpdate::StreamDone)
        }),
    )
    .await
    .expect("root should finish the final combined answer")
}

fn successful_subagent_finish(
    run_id: uuid::Uuid,
    call_id: uuid::Uuid,
) -> ylva::subagent_manager::FinishResult {
    ylva::subagent_manager::FinishResult {
        run_id,
        call_id,
        origin: ylva::subagent_manager::FinishResultOrigin::Agent,
        status: "success".to_string(),
        result: Some("subagent result".to_string()),
        error: None,
        sources: vec![],
    }
}

fn tool_progress_run_id(update: &UiUpdate, expected_status: &str) -> Option<uuid::Uuid> {
    match update {
        UiUpdate::ToolProgress {
            title_arguments, ..
        } => {
            let status_matches = title_arguments
                .iter()
                .any(|(key, value)| key == "status" && value == expected_status);
            status_matches.then(|| {
                title_arguments
                    .iter()
                    .find(|(key, _)| key == "run_id")
                    .and_then(|(_, value)| value.as_str())
                    .and_then(|value| value.parse().ok())
            })?
        }
        _ => None,
    }
}

#[tokio::test(start_paused = true)]
async fn intermediate_subagent_completion_does_not_reprompt_until_all_subagents_finish() {
    use std::sync::Mutex;

    use ylva::subagent_manager::{FinishResult, SpawnFn, SubagentManager};

    let registry = Arc::new(
        default_registry(
            make_sandbox(),
            ToolRegistryConfig {
                include_spawn_subagent: true,
                auto_approve: std::collections::HashSet::from([AutoApproveTool::Spawn]),
                ..ToolRegistryConfig::default()
            },
        )
        .expect("valid sandbox fixture"),
    );
    let interval = Duration::from_millis(5);
    let provider = SequencedMockProvider::new(
        vec![
            vec![
                call(
                    "spawn_subagent",
                    0,
                    serde_json::json!({
                        "label": "First subagent",
                        "task": "first task"
                    }),
                ),
                call(
                    "spawn_subagent",
                    1,
                    serde_json::json!({
                        "label": "Second subagent",
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

    let (finish_tx, finish_rx) = mpsc::unbounded_channel();
    let (subagent_ui_tx, subagent_ui_rx) = mpsc::unbounded_channel();
    let spawned_runs = Arc::new(Mutex::new(Vec::new()));
    let spawned_runs_clone = Arc::clone(&spawned_runs);
    let spawn_fn: SpawnFn = Box::new(move |request, _cancel, _ui_tx| {
        spawned_runs_clone
            .lock()
            .unwrap()
            .push((request.run_id, request.call_id, request.label));
        ylva::subagent_manager::SpawnOutcome {
            command_sender: None,
            handle: tokio::spawn(async {}),
        }
    });
    let manager = SubagentManager::new(subagent_ui_tx, spawn_fn, Some(1));
    agent = agent.with_subagent_manager(manager, finish_rx, subagent_ui_rx);

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
            UiUpdate::SubagentStarted {
                run_id, call_id, ..
            } => Some((*run_id, *call_id)),
            _ => None,
        })
        .expect("one subagent should have started");
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
        .expect("one subagent should be queued");
    assert_ne!(running_call_id, second_call_id);
    assert_eq!(spawned_runs.lock().unwrap().len(), 1);

    finish_tx
        .send(FinishResult {
            run_id: running_run_id,
            call_id: running_call_id,
            origin: ylva::subagent_manager::FinishResultOrigin::Agent,
            status: "success".to_string(),
            result: Some("first result".to_string()),
            error: None,
            sources: vec![],
        })
        .expect("first subagent completion should be accepted");
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
            origin: ylva::subagent_manager::FinishResultOrigin::Agent,
            status: "success".to_string(),
            result: Some("second result".to_string()),
            error: None,
            sources: vec![],
        })
        .expect("second subagent completion should be accepted");
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
        .any(|notice| notice.contains("\"label\":\"First subagent\"")));
    assert!(completion_notices
        .iter()
        .any(|notice| notice.contains("\"label\":\"Second subagent\"")));
}

#[tokio::test(start_paused = true)]
async fn interleaved_subagent_tokens_retain_their_own_run_id() {
    use std::collections::HashMap;

    use uuid::Uuid;
    use ylva::subagent_manager::{FinishResult, SpawnFn, SubagentManager};

    let registry = Arc::new(
        default_registry(
            make_sandbox(),
            ToolRegistryConfig {
                include_spawn_subagent: true,
                auto_approve: std::collections::HashSet::from([AutoApproveTool::Spawn]),
                ..ToolRegistryConfig::default()
            },
        )
        .expect("valid sandbox fixture"),
    );
    let interval = Duration::from_millis(5);
    let provider = SequencedMockProvider::new(
        vec![
            vec![
                call(
                    "spawn_subagent",
                    0,
                    serde_json::json!({"label": "First subagent", "task": "first task"}),
                ),
                call(
                    "spawn_subagent",
                    1,
                    serde_json::json!({"label": "Second subagent", "task": "second task"}),
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

    let (finish_tx, finish_rx) = mpsc::unbounded_channel();
    let (subagent_ui_tx, subagent_ui_rx) = mpsc::unbounded_channel();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let spawn_barrier = Arc::clone(&barrier);
    let finish_gate = Arc::new(tokio::sync::Semaphore::new(0));
    let spawn_finish_gate = Arc::clone(&finish_gate);
    let spawn_finish_tx = finish_tx.clone();
    let spawn_fn: SpawnFn = Box::new(move |request, _cancel, forward_ui_tx| {
        let run_id = request.run_id;
        let call_id = request.call_id;
        let task = request.task;
        let barrier = Arc::clone(&spawn_barrier);
        let finish_gate = Arc::clone(&spawn_finish_gate);
        let finish_tx = spawn_finish_tx.clone();
        let handle = tokio::spawn(async move {
            let _ = forward_ui_tx.send(UiUpdate::SubagentToken {
                run_id,
                text: format!("{task} first chunk"),
                is_thinking: false,
            });
            barrier.wait().await;
            let _ = forward_ui_tx.send(UiUpdate::SubagentToken {
                run_id,
                text: format!("{task} thinking"),
                is_thinking: true,
            });
            let permit = finish_gate
                .acquire()
                .await
                .expect("test keeps the subagent completion gate open");
            permit.forget();
            let _ = finish_tx.send(FinishResult {
                run_id,
                call_id,
                origin: ylva::subagent_manager::FinishResultOrigin::Agent,
                status: "success".to_string(),
                result: Some(format!("completed {task}")),
                error: None,
                sources: vec![],
            });
        });
        ylva::subagent_manager::SpawnOutcome {
            command_sender: None,
            handle,
        }
    });
    let manager = SubagentManager::new(subagent_ui_tx, spawn_fn, Some(2));
    agent = agent.with_subagent_manager(manager, finish_rx, subagent_ui_rx);

    agent
        .send_command(AgentCommand::Prompt("delegate twice".to_string()))
        .await;
    let handle = agent.run();

    let mut subagent_token_count = 0;
    let mut stream_done_count = 0;
    let mut observed_updates = Vec::new();
    let mut updates = tokio::time::timeout(
        Duration::from_secs(5),
        collect_until_advancing(&mut ui_rx, interval, |update| {
            observed_updates.push(format!("{update:?}"));
            if matches!(update, UiUpdate::SubagentToken { .. }) {
                subagent_token_count += 1;
            }
            if matches!(update, UiUpdate::StreamDone) {
                stream_done_count += 1;
            }
            subagent_token_count == 4 && stream_done_count == 1
        }),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "bounded wait expired: subagent_token_count={subagent_token_count}, \
             stream_done_count={stream_done_count}, updates={observed_updates:?}"
        )
    });

    let task_by_run_id: HashMap<_, _> = updates
        .iter()
        .filter_map(|update| match update {
            UiUpdate::SubagentStarted { run_id, task, .. } => Some((*run_id, task.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(
        task_by_run_id.len(),
        2,
        "both subagents started concurrently"
    );
    let subagent_tokens: Vec<(Uuid, String, bool)> = updates
        .iter()
        .filter_map(|update| match update {
            UiUpdate::SubagentToken {
                run_id,
                text,
                is_thinking,
            } => Some((*run_id, text.clone(), *is_thinking)),
            _ => None,
        })
        .collect();
    assert_eq!(subagent_tokens.len(), 4);
    for (run_id, text, is_thinking) in &subagent_tokens {
        let task = task_by_run_id
            .get(run_id)
            .expect("every token belongs to a started run");
        assert!(text.starts_with(task), "{text:?} attributed to {task:?}");
        assert!(
            text.ends_with(if *is_thinking {
                "thinking"
            } else {
                "first chunk"
            }),
            "stream kind must match the emitted token: {text:?}"
        );
    }
    let first_token_run_ids: Vec<Uuid> = subagent_tokens
        .iter()
        .filter(|(_, text, _)| text.ends_with("first chunk"))
        .map(|(run_id, _, _)| *run_id)
        .collect();
    assert_eq!(first_token_run_ids.len(), 2);
    assert_ne!(
        first_token_run_ids[0], first_token_run_ids[1],
        "both subagent streams must contribute before either reaches its second chunk"
    );
    finish_gate.add_permits(2);
    let final_updates = tokio::time::timeout(
        Duration::from_secs(5),
        collect_until_advancing(&mut ui_rx, interval, |update| {
            if matches!(update, UiUpdate::StreamDone) {
                stream_done_count += 1;
            }
            stream_done_count == 2
        }),
    )
    .await
    .expect("root should finish its single final continuation after both subagents finish");
    updates.extend(final_updates);
    assert_eq!(
        updates
            .iter()
            .filter(|update| matches!(update, UiUpdate::StreamStarted))
            .count(),
        3,
        "the root is re-prompted once after the final subagent finishes"
    );

    drop(agent);
    handle.await.expect("loop ok").expect("context");
}

#[tokio::test(start_paused = true)]
async fn subagent_completion_is_delivered_after_the_subagent_tool_done_that_precedes_it() {
    let ConcurrencyScenario {
        mut agent,
        mut ui_rx,
        finish_tx,
        spawned_requests: _,
        subagent_ui_senders,
        interval,
    } = build_concurrency_scenario(Some(1), 1);
    agent
        .send_command(AgentCommand::Prompt("delegate one task".to_string()))
        .await;
    let handle = agent.run();

    let updates = collect_through_working_response(&mut ui_rx, interval).await;
    let (run_id, call_id) = updates
        .iter()
        .find_map(|update| match update {
            UiUpdate::SubagentStarted {
                run_id, call_id, ..
            } => Some((*run_id, *call_id)),
            _ => None,
        })
        .expect("the subagent must start");

    let subagent_tool_id = uuid::Uuid::new_v4();
    let subagent_ui_tx = subagent_ui_senders.lock().expect("subagent UI senders")[0].clone();
    subagent_ui_tx
        .send(UiUpdate::ToolDone {
            id: subagent_tool_id,
            name: "read_file".to_string(),
            result_section: ylva::tui_reducer_state::ResultSection::plain("subagent output"),
            summary: vec![],
            duration: Duration::from_millis(1),
            content_sections: vec![],
            run_id: Some(run_id),
        })
        .expect("subagent tool completion accepted");
    finish_tx
        .send(successful_subagent_finish(run_id, call_id))
        .expect("subagent finish accepted");

    let completion_updates = tokio::time::timeout(
        Duration::from_secs(5),
        collect_until_advancing(&mut ui_rx, interval, |update| {
            matches!(update, UiUpdate::SubagentDone { run_id: done_run_id, .. } if *done_run_id == run_id)
        }),
    )
    .await
    .expect("subagent completion is delivered");
    let subagent_tool_done_index = completion_updates.iter().position(
        |update| matches!(update, UiUpdate::ToolDone { id, .. } if *id == subagent_tool_id),
    );
    let subagent_done_index = completion_updates.iter().position(
        |update| matches!(update, UiUpdate::SubagentDone { run_id: done_run_id, .. } if *done_run_id == run_id),
    );
    assert!(
        matches!((subagent_tool_done_index, subagent_done_index), (Some(tool_done), Some(done)) if tool_done < done),
        "the subagent's ToolDone must be delivered before its SubagentDone"
    );

    collect_final_root_response(&mut ui_rx, interval).await;
    drop(agent);
    handle.await.expect("loop ok").expect("context");
}

#[tokio::test(start_paused = true)]
async fn limit_two_starts_the_third_subagent_after_one_finishes_and_reprompts_after_the_last() {
    let ConcurrencyScenario {
        mut agent,
        mut ui_rx,
        finish_tx,
        spawned_requests,
        subagent_ui_senders: _,
        interval,
    } = build_concurrency_scenario(Some(2), 3);
    agent
        .send_command(AgentCommand::Prompt("delegate three tasks".to_string()))
        .await;
    let handle = agent.run();

    let mut updates = collect_through_working_response(&mut ui_rx, interval).await;
    let initially_running: Vec<_> = updates
        .iter()
        .filter_map(|update| match update {
            UiUpdate::SubagentStarted {
                run_id, call_id, ..
            } => Some((*run_id, *call_id)),
            _ => None,
        })
        .collect();
    assert_eq!(initially_running.len(), 2);
    let (queued_call_id, queued_run_id) = updates
        .iter()
        .find_map(|update| match update {
            UiUpdate::ToolProgress {
                id,
                title_arguments,
            } if tool_progress_run_id(update, "queued").is_some() => {
                Some((*id, tool_progress_run_id(update, "queued").unwrap()))
            }
            _ => None,
        })
        .expect("the third subagent must be queued at the limit");
    assert_eq!(spawned_requests.lock().unwrap().len(), 2);

    let (first_run_id, first_call_id) = initially_running[0];
    finish_tx
        .send(successful_subagent_finish(first_run_id, first_call_id))
        .expect("first subagent finish accepted");
    let mut first_subagent_done = false;
    let mut queued_subagent_started = None;
    let after_first_finish = tokio::time::timeout(
        Duration::from_secs(5),
        collect_until_advancing(&mut ui_rx, interval, |update| {
            if matches!(update, UiUpdate::ToolDone { id, .. } if *id == first_call_id) {
                first_subagent_done = true;
            }
            if let UiUpdate::SubagentStarted {
                run_id, call_id, ..
            } = update
            {
                if *call_id == queued_call_id {
                    queued_subagent_started = Some((*run_id, *call_id));
                }
            }
            first_subagent_done && queued_subagent_started.is_some()
        }),
    )
    .await
    .expect("the queued third subagent should start as the first subagent finishes");
    updates.extend(after_first_finish);
    assert_eq!(
        queued_subagent_started,
        Some((queued_run_id, queued_call_id))
    );
    let third_request = spawned_requests
        .lock()
        .unwrap()
        .iter()
        .find(|request| request.run_id == queued_run_id)
        .cloned()
        .expect("the queued request should now be running");

    let (second_run_id, second_call_id) = initially_running[1];
    finish_tx
        .send(successful_subagent_finish(second_run_id, second_call_id))
        .expect("second subagent finish accepted");
    let second_done = tokio::time::timeout(
        Duration::from_secs(5),
        collect_until_advancing(
            &mut ui_rx,
            interval,
            |update| matches!(update, UiUpdate::ToolDone { id, .. } if *id == second_call_id),
        ),
    )
    .await
    .expect("second subagent should finish without a root continuation");
    updates.extend(second_done);

    finish_tx
        .send(successful_subagent_finish(
            third_request.run_id,
            third_request.call_id,
        ))
        .expect("third subagent finish accepted");
    let final_updates = collect_final_root_response(&mut ui_rx, interval).await;
    updates.extend(final_updates);
    assert_eq!(
        updates
            .iter()
            .filter(|update| matches!(update, UiUpdate::StreamStarted))
            .count(),
        3,
        "the root receives one continuation only after the last subagent finishes"
    );

    drop(agent);
    handle.await.expect("loop ok").expect("context");
}

async fn assert_concurrency_cap_starts_all_before_finishing(
    max_concurrent_subagents: Option<usize>,
    subagent_count: usize,
) {
    let ConcurrencyScenario {
        mut agent,
        mut ui_rx,
        finish_tx,
        spawned_requests,
        subagent_ui_senders: _,
        interval,
    } = build_concurrency_scenario(max_concurrent_subagents, subagent_count);
    agent
        .send_command(AgentCommand::Prompt(
            "delegate independent tasks".to_string(),
        ))
        .await;
    let handle = agent.run();

    let mut updates = collect_through_working_response(&mut ui_rx, interval).await;
    let started_subagents: Vec<_> = updates
        .iter()
        .filter_map(|update| match update {
            UiUpdate::SubagentStarted {
                run_id, call_id, ..
            } => Some((*run_id, *call_id)),
            _ => None,
        })
        .collect();
    assert_eq!(
        started_subagents.len(),
        subagent_count,
        "all subagents start before any finish when the cap is sufficient"
    );
    assert_eq!(spawned_requests.lock().unwrap().len(), subagent_count);
    assert!(
        updates
            .iter()
            .all(|update| tool_progress_run_id(update, "queued").is_none()),
        "a sufficiently high cap should leave no subagent queued"
    );

    for (run_id, call_id) in started_subagents {
        finish_tx
            .send(successful_subagent_finish(run_id, call_id))
            .expect("subagent finish accepted");
        let finished = tokio::time::timeout(
            Duration::from_secs(5),
            collect_until_advancing(
                &mut ui_rx,
                interval,
                |update| matches!(update, UiUpdate::ToolDone { id, .. } if *id == call_id),
            ),
        )
        .await
        .expect("each subagent should reach its terminal update");
        updates.extend(finished);
    }
    updates.extend(collect_final_root_response(&mut ui_rx, interval).await);
    assert_eq!(
        updates
            .iter()
            .filter(|update| matches!(update, UiUpdate::StreamStarted))
            .count(),
        3,
        "the root is continued once after the final completion, not after intermediates"
    );

    drop(agent);
    handle.await.expect("loop ok").expect("context");
}

#[tokio::test(start_paused = true)]
async fn limit_three_runs_all_three_subagents_concurrently_and_reprompts_once() {
    assert_concurrency_cap_starts_all_before_finishing(Some(3), 3).await;
}

#[tokio::test(start_paused = true)]
async fn unlimited_starts_all_five_subagents_concurrently() {
    assert_concurrency_cap_starts_all_before_finishing(None, 5).await;
}

#[tokio::test(start_paused = true)]
async fn iteration_guard_also_stops_a_runaway_spawn_subagent_finish_reprompt_loop() {
    // Regression test: a subagent that resolves near-instantly (finishes via
    // `finish_tx` essentially as soon as it's spawned) drives the root
    // through the `finish_rx` "root was idle, forcing reprompt" branch
    // instead of `advance_tool_queue`'s queue-empty branch. That branch used
    // to call `start_request` directly with no iteration bookkeeping at all
    // — so a model that just keeps calling `spawn_subagent` again on every
    // finish notice (rather than ever answering) could reprompt forever,
    // one HTTP request per "finish", completely bypassing
    // `DEFAULT_MAX_TOOL_USE_ITERATIONS`. The guard must trip here exactly like it
    // does for the plain `write`-in-a-loop case above.
    use tokio_util::sync::CancellationToken;
    use ylva::subagent_manager::{FinishResult, SpawnFn, SubagentManager};

    let registry = Arc::new(
        default_registry(
            make_sandbox(),
            ToolRegistryConfig {
                include_spawn_subagent: true,
                auto_approve: std::collections::HashSet::from([AutoApproveTool::Spawn]),
                ..ToolRegistryConfig::default()
            },
        )
        .expect("valid sandbox fixture"),
    );
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

    let (finish_tx, finish_rx) = mpsc::unbounded_channel();
    let (subagent_ui_tx, subagent_ui_rx) = mpsc::unbounded_channel();
    // Resolves immediately: reports success via `finish_tx` right away and
    // never actually does any work, simulating a subagent that finishes near-
    // instantly relative to the root's own request/response cycle.
    let spawn_fn: SpawnFn = Box::new(move |request, _cancel: CancellationToken, _ui_tx| {
        let finish_tx = finish_tx.clone();
        let handle = tokio::spawn(async move {
            let _ = finish_tx.send(FinishResult {
                run_id: request.run_id,
                call_id: request.call_id,
                origin: ylva::subagent_manager::FinishResultOrigin::Agent,
                status: "success".to_string(),
                result: Some("done".to_string()),
                error: None,
                sources: vec![],
            });
        });
        ylva::subagent_manager::SpawnOutcome {
            command_sender: None,
            handle,
        }
    });
    let manager = SubagentManager::new(subagent_ui_tx, spawn_fn, Some(1));
    agent = agent.with_subagent_manager(manager, finish_rx, subagent_ui_rx);

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
async fn stop_cancels_multiple_running_and_queued_subagents() {
    // Stop must resolve every running and queued spawn bubble to a terminal
    // cancelled result; only the first two subagent tasks are constructed under
    // the configured limit, while the third remains queued.
    use tokio_util::sync::CancellationToken;
    use ylva::subagent_manager::{SpawnFn, SubagentManager};

    let registry = Arc::new(
        default_registry(
            make_sandbox(),
            ToolRegistryConfig {
                include_spawn_subagent: true,
                auto_approve: std::collections::HashSet::from([AutoApproveTool::Spawn]),
                ..ToolRegistryConfig::default()
            },
        )
        .expect("valid sandbox fixture"),
    );
    let interval = Duration::from_millis(5);
    let provider = SequencedMockProvider::new(
        vec![
            vec![
                call(
                    "spawn_subagent",
                    0,
                    serde_json::json!({ "label": "First subagent", "task": "first task" }),
                ),
                call(
                    "spawn_subagent",
                    1,
                    serde_json::json!({ "label": "Second subagent", "task": "second task" }),
                ),
                call(
                    "spawn_subagent",
                    2,
                    serde_json::json!({ "label": "Third subagent", "task": "third task" }),
                ),
            ],
            vec![MockEvent::Text("working".to_string())],
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

    let (finish_tx, finish_rx) = mpsc::unbounded_channel();
    let (subagent_ui_tx, subagent_ui_rx) = mpsc::unbounded_channel();
    // Never actually finishes on its own — only cancellation ends it, so the
    // test can observe `Stop` resolving the bubble rather than a real finish.
    let spawn_fn: SpawnFn = Box::new(move |_request, cancel: CancellationToken, _ui_tx| {
        let handle = tokio::spawn(async move {
            cancel.cancelled().await;
        });
        ylva::subagent_manager::SpawnOutcome {
            command_sender: None,
            handle,
        }
    });
    let manager = SubagentManager::new(subagent_ui_tx, spawn_fn, Some(2));
    agent = agent.with_subagent_manager(manager, finish_rx, subagent_ui_rx);
    drop(finish_tx);

    agent
        .send_command(AgentCommand::Prompt("delegate it".into()))
        .await;
    let handle = agent.run();
    let initial_updates = collect_through_working_response(&mut ui_rx, interval).await;
    let running_call_ids: Vec<_> = initial_updates
        .iter()
        .filter_map(|update| match update {
            UiUpdate::SubagentStarted { call_id, .. } => Some(*call_id),
            _ => None,
        })
        .collect();
    assert_eq!(running_call_ids.len(), 2);
    let queued_call_id = initial_updates
        .iter()
        .find_map(|update| match update {
            UiUpdate::ToolProgress { id, .. }
                if tool_progress_run_id(update, "queued").is_some() =>
            {
                Some(*id)
            }
            _ => None,
        })
        .expect("the third subagent should be queued");
    let spawn_call_ids: std::collections::HashSet<_> = running_call_ids
        .iter()
        .copied()
        .chain([queued_call_id])
        .collect();
    assert_eq!(spawn_call_ids.len(), 3);

    agent
        .send_command(AgentCommand::Stop {
            reason: "aborted by user".into(),
        })
        .await;
    let mut cancelled_call_ids = Vec::new();
    let _done = tokio::time::timeout(
        Duration::from_secs(5),
        collect_until_advancing(&mut ui_rx, interval, |update| {
            if let UiUpdate::ToolDone {
                id, name, summary, ..
            } = update
            {
                if name == "spawn_subagent"
                    && summary
                        .iter()
                        .any(|(key, value)| key == "status" && value == "cancelled")
                {
                    cancelled_call_ids.push(*id);
                }
            }
            cancelled_call_ids.len() == 3
        }),
    )
    .await
    .expect("Stop should settle all three spawn calls as cancelled");
    assert_eq!(cancelled_call_ids.len(), 3);
    assert_eq!(
        cancelled_call_ids
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>(),
        spawn_call_ids,
        "each running and queued spawn call gets its own cancelled completion"
    );

    // Force termination after the observable cancellation results; the test
    // does not need to run the root's post-stop turn.
    drop(ui_rx);
    drop(agent);
    let context = handle.await.expect("loop ok").expect("context");
    let cancelled_notices = context
        .messages()
        .iter()
        .filter(|message| {
            matches!(
                message,
                Message::User { content, kind: UserMessageKind::ImplicitToolNote }
                    if content.contains("spawn_subagent")
                        && content.contains("\"event\":\"subagent_completed\"")
                        && content.contains("\"status\":\"cancelled\"")
            )
        })
        .count();
    assert!(
        cancelled_notices == 3,
        "expected one implicit cancellation completion notice per subagent, got: {:?}",
        context.messages(),
    );
}

#[tokio::test(start_paused = true)]
async fn for_subagent_targeting_an_unknown_run_id_is_dropped_without_hanging_the_root() {
    // Regression test for `AgentCommand::ForSubagent`'s "stale target" case: a
    // command addressed to a `run_id` the root's `SubagentManager` doesn't
    // recognize (already finished, or never existed) must be logged and
    // dropped, not panic or silently wedge the root loop's `select!` — the
    // root must stay fully responsive to its own, unrelated commands
    // afterwards.
    let registry = Arc::new(
        default_registry(make_sandbox(), ToolRegistryConfig::default())
            .expect("valid sandbox fixture"),
    );
    let interval = Duration::from_millis(5);
    let provider = MockProvider::with_script(vec![MockEvent::Text("hi there".into())], interval);
    let (mut agent, mut ui_rx) = AgentLoop::new(
        Arc::new(EngineImpl::new(
            Arc::new(provider),
            common::test_engine_config(),
        )),
        Arc::clone(&registry),
    );

    // No `SubagentManager` is wired at all, so this is the strongest form
    // of "no live subagent recognizes this run_id".
    let stray_run_id = uuid::Uuid::new_v4();
    agent
        .send_command(AgentCommand::ForSubagent {
            run_id: stray_run_id,
            command: Box::new(AgentCommand::ApproveTool { id: stray_run_id }),
        })
        .await;

    let handle = agent.run();

    // The root must still be able to run a completely ordinary prompt
    // afterwards — if the stray `ForSubagent` had wedged the `select!` loop,
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
         ForSubagent, got: {updates:?}"
    );

    drop(agent);
    handle.await.expect("loop ok").expect("context");
}
