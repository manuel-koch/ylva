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
async fn iteration_guard_honors_a_custom_child_limit() {
    let registry = Arc::new(default_registry(
        make_sandbox(),
        ToolRegistryConfig::default(),
    ));
    let interval = Duration::from_millis(0);
    let provider = MockProvider::with_script(
        vec![call("read", 0, serde_json::json!({ "path": "Cargo.toml" }))],
        interval,
    );
    let (finish_tx, _finish_rx) = mpsc::channel(1);
    let (subagent_ui_tx, _subagent_ui_rx) = mpsc::channel(16);
    let run_id = uuid::Uuid::new_v4();
    let call_id = uuid::Uuid::new_v4();

    let (agent, mut ui_rx) = AgentLoop::new(
        Arc::new(EngineImpl::new(
            Arc::new(provider),
            common::test_engine_config(),
        )),
        Arc::clone(&registry),
    );
    let mut agent = agent
        .with_child_role(ylva::ChildRoleConfig {
            run_id,
            call_id,
            session_id: None,
            briefing: None,
            parent_cancel: CancellationToken::new(),
            subagent_ui_tx,
            finish_tx,
        })
        .with_max_tool_iterations(2);

    agent
        .send_command(AgentCommand::Prompt("child loop".into()))
        .await;
    let mut handle = agent.run();

    // Drain `ui_rx` while `run()` is in flight, exactly like the drivers do:
    // the bounded channel fills long before the cap trips otherwise and the
    // loop parks on `send().await` (see docs/known-test-issues.md).
    let mut collected_updates: Vec<UiUpdate> = Vec::new();
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                biased;
                joined = &mut handle => break joined,
                update = ui_rx.recv() => match update {
                    Some(update) => collected_updates.push(update),
                    None => std::future::pending::<()>().await,
                },
            }
        }
    })
    .await
    .expect("the child returns once the custom iteration cap trips")
    .expect("run() task should not panic");

    // Collect anything `run()` posted just before returning that the select
    // loop above had not polled yet (both branches were ready, and `biased`
    // preferred `handle`).
    while let Ok(update) = ui_rx.try_recv() {
        collected_updates.push(update);
    }

    let error = result.expect_err("a child hitting its cap must return Err");
    assert!(
        error.to_string().contains("Max tool-use iterations (2)"),
        "the child error must report the configured limit; got: {error}"
    );
    assert!(
        collected_updates.iter().any(
            |u| matches!(u, UiUpdate::Error { message, run_id: Some(error_run_id) }
                if error_run_id == &run_id && message.contains("Max tool-use iterations (2)"))
        ),
        "the guard error must be attributed to this child run"
    );
}

#[tokio::test(start_paused = true)]
async fn intermediate_subagent_completion_does_not_reprompt_until_all_children_finish() {
    use std::sync::Mutex;

    use ylva::child_agent_manager::{ChildAgentManager, FinishResult, SpawnFn};

    let registry = Arc::new(default_registry(
        make_sandbox(),
        ToolRegistryConfig {
            include_spawn_subagent: true,
            auto_approve: std::collections::HashSet::from([AutoApproveTool::Spawn]),
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
            origin: ylva::child_agent_manager::FinishResultOrigin::Agent,
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
            origin: ylva::child_agent_manager::FinishResultOrigin::Agent,
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
    // `DEFAULT_MAX_TOOL_USE_ITERATIONS`. The guard must trip here exactly like it
    // does for the plain `write`-in-a-loop case above.
    use tokio_util::sync::CancellationToken;
    use ylva::child_agent_manager::{ChildAgentManager, FinishResult, SpawnFn};

    let registry = Arc::new(default_registry(
        make_sandbox(),
        ToolRegistryConfig {
            include_spawn_subagent: true,
            auto_approve: std::collections::HashSet::from([AutoApproveTool::Spawn]),
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
                    origin: ylva::child_agent_manager::FinishResultOrigin::Agent,
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
            auto_approve: std::collections::HashSet::from([AutoApproveTool::Spawn]),
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
    drop(ui_rx);
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
