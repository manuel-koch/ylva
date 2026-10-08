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

#[tokio::test]
async fn compaction_marker_emission_preserves_the_owning_run() {
    let (ui_tx, mut ui_rx) = mpsc::channel(4);
    let mut context = Context::new();
    for message_index in 0..15 {
        context.push_user(format!("Message {message_index}"));
    }
    let boundary = context.messages().len();
    for run_id in [None, Some(Uuid::new_v4())] {
        send_compaction_applied(&ui_tx, &context, boundary, run_id).await;
        let update = ui_rx.recv().await.expect("marker was emitted");
        let UiUpdate::CompactionApplied {
            run_id: marker_run_id,
            entry_index,
            summarized_messages,
        } = update
        else {
            panic!("expected a compaction marker, got {update:?}");
        };
        assert_eq!(marker_run_id, run_id);
        assert_eq!(summarized_messages, 15);
        assert_eq!(
            entry_index,
            crate::tui_reducer_state::project_messages(context.messages()).len()
        );
    }
}

#[test]
fn reports_background_compaction_start_in_the_transcript() {
    crate::test_support::run_locked(async {
        let (ui_tx, mut ui_rx) = mpsc::channel(4);
        let mut context = Context::new();
        context.push_user("word ".repeat(10_000));
        context.push_assistant("recorded answer ".repeat(100), AssistantStatus::Complete);
        let active_engine = test_engine_with_context_length(16_000);
        let compact_engine = test_engine();
        let (mut compaction, _outcome_rx) = compaction::CompactionRuntime::new(
            Some(compact_engine),
            compaction::COMPACTION_SUMMARY_PROMPT.to_string(),
            Duration::from_secs(compaction::DEFAULT_COMPACTION_TIMEOUT_SECS),
        );

        sweep_compaction_and_report(
            &mut compaction,
            &mut context,
            active_engine.as_ref(),
            &[],
            0,
            None,
            &ui_tx,
        )
        .await;

        assert!(matches!(
            ui_rx.recv().await,
            Some(UiUpdate::Notice { text, run_id: None })
                if text == "Started background context compaction."
        ));
    });
}

#[test]
fn reports_summary_before_synchronous_drop_in_the_same_sweep() {
    crate::test_support::run_locked(async {
        let (ui_tx, mut ui_rx) = mpsc::channel(8);
        let mut context = Context::new();
        context.push_user("old context".to_string());
        let summary_boundary = context.messages().len();
        context.push_user("word ".repeat(20_000));
        context.push_user("current task".to_string());
        let active_engine = test_engine_with_context_length(16_000);
        let (mut compaction, _outcome_rx) = compaction::CompactionRuntime::new(
            Some(test_engine()),
            compaction::COMPACTION_SUMMARY_PROMPT.to_string(),
            Duration::from_secs(compaction::DEFAULT_COMPACTION_TIMEOUT_SECS),
        );
        compaction.on_outcome(
            compaction::CompactionOutcome::SummaryReady {
                generation: 0,
                request_id: None,
                boundary: summary_boundary,
                summary: "Earlier work.".to_string(),
            },
            0,
        );
        sweep_compaction_and_report(
            &mut compaction,
            &mut context,
            active_engine.as_ref(),
            &[],
            0,
            None,
            &ui_tx,
        )
        .await;
        assert!(matches!(
            ui_rx.recv().await,
            Some(UiUpdate::Notice { text, .. })
                if text.starts_with("Background context compaction completed.")
        ));
        assert!(matches!(
            ui_rx.recv().await,
            Some(UiUpdate::Notice { text, .. })
                if text.starts_with("Compacted context synchronously")
        ));
    });
}

#[test]
fn reports_background_compaction_completion_with_detail_loss_guidance() {
    crate::test_support::run_locked(async {
        let (ui_tx, mut ui_rx) = mpsc::channel(8);
        let mut context = Context::new();
        context.push_user("old context".to_string());
        let active_engine = test_engine_with_context_length(16_000);
        let (mut compaction, _outcome_rx) = compaction::CompactionRuntime::new(
            Some(test_engine()),
            compaction::COMPACTION_SUMMARY_PROMPT.to_string(),
            Duration::from_secs(compaction::DEFAULT_COMPACTION_TIMEOUT_SECS),
        );
        let run_id = Some(Uuid::new_v4());
        compaction.on_outcome(
            compaction::CompactionOutcome::SummaryReady {
                generation: 0,
                request_id: None,
                boundary: context.messages().len(),
                summary: "Earlier work.".to_string(),
            },
            0,
        );
        sweep_compaction_and_report(
            &mut compaction,
            &mut context,
            active_engine.as_ref(),
            &[],
            0,
            run_id,
            &ui_tx,
        )
        .await;
        assert!(matches!(
            ui_rx.recv().await,
            Some(UiUpdate::Notice { text, run_id: notice_run_id })
                if notice_run_id == run_id
                    && text.starts_with("Background context compaction completed.")
                    && text.contains("the full transcript is preserved")
                    && text.contains("If compaction becomes frequent")
                    && text.contains("narrowing the task")
                    && text.contains("concise handoff")
                    && text.contains("repeated compaction may lose detail")
        ));
    });
}

#[test]
fn reports_compaction_failure_and_refreshes_model_view_usage() {
    crate::test_support::run_locked(async {
        let (ui_tx, mut ui_rx) = mpsc::channel(8);
        let mut context = Context::new();
        context.push_user("old context ".repeat(4_000));
        context.push_assistant("old answer".to_string(), AssistantStatus::Complete);
        context.push_user("current task".to_string());
        let active_engine = test_engine_with_context_length(16_000);
        let compact_engine = test_engine();
        let (mut compaction, mut outcome_rx) = compaction::CompactionRuntime::new(
            Some(compact_engine),
            compaction::COMPACTION_SUMMARY_PROMPT.to_string(),
            Duration::from_secs(compaction::DEFAULT_COMPACTION_TIMEOUT_SECS),
        );
        ui_tx
            .send(UiUpdate::ContextUsage {
                estimated_tokens: 12_000,
                context_length: 16_000,
                source: ContextUsageSource::SerializedRequest,
            })
            .await
            .expect("seed stale usage");
        // Drive the real reporting function's post-compaction path using
        // a forced fallback outcome at the next safe boundary.
        compaction.on_outcome(
            compaction::CompactionOutcome::SummaryFailed {
                generation: 0,
                request_id: None,
                reason: "compaction summary timed out after 60 seconds".to_string(),
            },
            0,
        );
        sweep_compaction_and_report(
            &mut compaction,
            &mut context,
            active_engine.as_ref(),
            &[],
            0,
            None,
            &ui_tx,
        )
        .await;

        let updates = std::iter::from_fn(|| ui_rx.try_recv().ok()).collect::<Vec<_>>();
        assert!(updates.iter().any(|update| matches!(
            update,
            UiUpdate::Notice { text, run_id: None }
                if text.contains("synchronously")
                    && text.contains("Older context was removed from the model view")
                    && text.contains("some details may no longer be available to the model")
                    && text.contains("The full transcript is preserved")
        )));
        assert!(updates.iter().any(|update| matches!(
            update,
            UiUpdate::ContextUsage {
                source: ContextUsageSource::ModelView,
                ..
            }
        )));
        let expected_usage = context.estimated_model_tokens(&[], active_engine.as_ref());
        let refreshed_usage = updates.iter().find_map(|update| match update {
            UiUpdate::ContextUsage {
                estimated_tokens,
                context_length,
                source: ContextUsageSource::ModelView,
            } => Some((*estimated_tokens, *context_length)),
            _ => None,
        });
        assert!(
            refreshed_usage == Some((expected_usage, 16_000)),
            "compaction reports the current model-view estimate: expected {expected_usage}"
        );
        assert!(outcome_rx.try_recv().is_err());
    });
}

#[test]
fn reports_compaction_failure_reason_as_transcript_notice() {
    crate::test_support::run_locked(async {
        let (ui_tx, mut ui_rx) = mpsc::channel(1);
        report_compaction_failure(
            &ui_tx,
            None,
            "compaction summary timed out after 60 seconds",
        )
        .await;

        assert!(matches!(
            ui_rx.recv().await,
            Some(UiUpdate::Notice { text, run_id: None })
                if text.contains("failed") && text.contains("timed out after 60 seconds")
        ));
    });
}

#[tokio::test(start_paused = true)]
async fn a_completed_turn_above_the_soft_threshold_compacts_the_model_view() {
    let prompt = sized_prompt();
    let context_length = first_turn_window(&prompt, 0.75);
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let active_config = EngineConfig {
        provider_connection: "mock".to_string(),
        model: "test-model".to_string(),
        temperature: None,
        context_length: Some(context_length),
        api_base_url: None,
        api_key: None,
        reasoning: None,
        compact_target_ratio: 0.5,
        soft_compaction_threshold: 0.7,
        hard_compaction_threshold: 0.9,
    };
    let active: Arc<dyn Engine> = Arc::new(EngineImpl::new(
        Arc::new(MockProvider::new(
            vec!["recorded answer ".repeat(100)],
            Duration::ZERO,
        )),
        active_config.clone(),
    ));
    let compact_engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
        Arc::new(ScriptedSummaryProvider {
            recorded: Arc::clone(&recorded),
            fail: false,
        }),
        active_config,
    ));
    let temporary_directory = tempfile::tempdir().expect("temporary directory");
    let store = crate::store::Store::create(
        &temporary_directory.path().join("chats"),
        temporary_directory.path(),
        Uuid::new_v4(),
        "mock",
        "test-model",
        None,
    )
    .expect("store created");

    let (agent_loop, mut ui_rx) = AgentLoop::new(
        active,
        Arc::new(tools::default_registry(
            test_sandbox(),
            tools::ToolRegistryConfig::default(),
        )),
    );
    let mut agent_loop = agent_loop
        .with_compaction_engine(compact_engine)
        .with_store(store.clone());
    let handle = agent_loop.run();
    agent_loop
        .send_command(AgentCommand::Prompt(prompt.clone()))
        .await;
    let _updates =
        collect_updates_until(&mut ui_rx, |update| matches!(update, UiUpdate::StreamDone)).await;
    settle_background_tasks().await;
    let post_compaction_usage =
        std::iter::from_fn(|| ui_rx.try_recv().ok()).find_map(|update| match update {
            UiUpdate::ContextUsage {
                estimated_tokens,
                context_length,
                source: ContextUsageSource::ModelView,
            } => Some((estimated_tokens, context_length)),
            _ => None,
        });
    assert!(
        post_compaction_usage.is_some(),
        "applying the background summary refreshes the footer with the model-view estimate"
    );

    let requests = recorded
        .lock()
        .expect("summary recording mutex is not poisoned")
        .clone();
    assert_eq!(
        requests.len(),
        1,
        "the completed turn above the soft threshold starts exactly one background summary"
    );
    let request = &requests[0];
    assert!(
        matches!(
            request.first(),
            Some(Message::System { content })
                if content.starts_with(compaction::COMPACTION_SUMMARY_PROMPT)
                    && content.contains("is the signal to produce the summary")
        ),
        "the summarizer gets its framing instruction first: {request:?}"
    );
    assert!(
        request.iter().any(|message| matches!(
            message,
            Message::User { content, .. } if content.as_str() == prompt.as_str()
        )),
        "the summarized prefix contains the original turn: {request:?}"
    );

    drop(agent_loop);
    let context = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("loop returns")
        .expect("loop task ok")
        .expect("loop returns a context");

    assert!(
        context.compaction().is_some(),
        "the background summary was applied at a safe boundary"
    );
    let snapshot = context.compaction().expect("compaction applied");
    assert_eq!(
        snapshot.summary,
        "Decided to keep the transcript in the store."
    );
    let model_messages = context.model_messages();
    let leading_system_len = context.leading_system_len();
    assert!(
        snapshot.boundary > leading_system_len && snapshot.boundary <= context.messages().len(),
        "the boundary sits inside the transcript: {}",
        snapshot.boundary
    );
    assert!(
        model_messages.iter().any(|message| matches!(
            message,
            Message::User { content, .. } if content.as_str() == prompt.as_str()
        )),
        "the explicit task anchor survives summarization: {model_messages:?}"
    );
    assert_eq!(
        &model_messages[..leading_system_len],
        &context.messages()[..leading_system_len],
        "the leading system instructions stay verbatim"
    );
    assert!(
        matches!(
            &model_messages[leading_system_len],
            Message::User { content, .. }
                if content.starts_with(compaction::COMPACTION_SUMMARY_MARKER)
        ),
        "the summary is projected as a marked user message: {model_messages:?}"
    );
    assert_eq!(
        &model_messages[leading_system_len + 2..],
        &context.messages()[snapshot.boundary..],
        "the tail after the boundary stays verbatim"
    );
    assert!(
        context.messages().iter().any(|message| matches!(
            message,
            Message::User { content, .. } if content.as_str() == prompt.as_str()
        )),
        "the transcript keeps every original message"
    );

    store.flush().await;
    let events = crate::store::Store::read_events(store.path()).expect("events readable");
    let compactions: Vec<(usize, String)> = events
        .iter()
        .filter_map(|event| match event {
            crate::store::StoreEvent::Compaction {
                boundary, summary, ..
            } => Some((*boundary, summary.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        compactions,
        vec![(
            snapshot.boundary,
            "Decided to keep the transcript in the store.".to_string()
        )],
        "the applied snapshot is appended to the session file"
    );
    assert_eq!(
        crate::store::Store::read_messages(store.path())
            .expect("messages readable")
            .iter()
            .filter(|message| matches!(message, Message::User { .. }))
            .count(),
        1,
        "restoring or applying the snapshot never writes the user turn twice"
    );

    let mut announced_boundary: Option<(usize, usize)> = None;
    while let Ok(update) = ui_rx.try_recv() {
        if let UiUpdate::CompactionApplied {
            run_id,
            entry_index,
            summarized_messages,
        } = update
        {
            assert_eq!(run_id, None, "root compaction retains its identity");
            announced_boundary = Some((entry_index, summarized_messages));
        }
    }
    let (entry_index, summarized_messages) =
        announced_boundary.expect("applying a snapshot announces the marker position to the TUI");
    assert_eq!(
        summarized_messages,
        snapshot.boundary - leading_system_len,
        "the marker counts transcript messages, not the rebuilt system prompts"
    );
    let store_messages =
        crate::store::Store::read_messages(store.path()).expect("messages readable");
    assert_eq!(
        entry_index,
        crate::tui_reducer_state::project_messages(&store_messages[..summarized_messages]).len(),
        "the emitted index matches what a resume-time replay computes"
    );
}

#[tokio::test(start_paused = true)]
async fn a_failed_background_summary_falls_back_to_dropping_at_the_next_boundary() {
    let prompt = sized_prompt();
    let context_length = first_turn_window(&prompt, 0.75);
    let active_config = EngineConfig {
        provider_connection: "mock".to_string(),
        model: "test-model".to_string(),
        temperature: None,
        context_length: Some(context_length),
        api_base_url: None,
        api_key: None,
        reasoning: None,
        compact_target_ratio: 0.5,
        soft_compaction_threshold: 0.7,
        hard_compaction_threshold: 0.9,
    };
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let active: Arc<dyn Engine> = Arc::new(EngineImpl::new(
        Arc::new(MockProvider::new(
            vec!["recorded answer ".repeat(100)],
            Duration::ZERO,
        )),
        active_config.clone(),
    ));
    let compact_engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
        Arc::new(ScriptedSummaryProvider {
            recorded: Arc::clone(&recorded),
            fail: true,
        }),
        active_config,
    ));

    let (agent_loop, mut ui_rx) = AgentLoop::new(
        active,
        Arc::new(tools::default_registry(
            test_sandbox(),
            tools::ToolRegistryConfig::default(),
        )),
    );
    let mut agent_loop = agent_loop.with_compaction_engine(compact_engine);
    let handle = agent_loop.run();
    agent_loop
        .send_command(AgentCommand::Prompt(prompt.clone()))
        .await;
    collect_updates_until(&mut ui_rx, |update| matches!(update, UiUpdate::StreamDone)).await;
    settle_background_tasks().await;

    assert_eq!(
        recorded
            .lock()
            .expect("summary recording mutex is not poisoned")
            .len(),
        1,
        "the summary attempt is made before the fallback runs"
    );

    drop(agent_loop);
    let context = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("loop returns")
        .expect("loop task ok")
        .expect("loop returns a context");

    assert!(
        context.compaction().is_none(),
        "a failed summary never becomes a snapshot"
    );
    assert!(
            context.model_messages().iter().any(|message| matches!(
                message,
                Message::User { content, .. }
                    if content == &prompt || content.contains(compaction::recovery::REQUEST_OMISSION_MARKER)
            )),
            "failed summarization preserves a task anchor instead of a system-only view"
        );
    assert!(
        matches!(
            context.model_messages().first(),
            Some(Message::System { .. })
        ),
        "the leading system instructions are never dropped"
    );
    assert!(
        context.messages().iter().any(|message| matches!(
            message,
            Message::User { content, .. } if content.as_str() == prompt.as_str()
        )),
        "the transcript keeps every original message"
    );
}

#[tokio::test(start_paused = true)]
async fn a_turn_above_the_hard_threshold_drops_without_starting_a_summary() {
    let prompt = sized_prompt();
    let context_length = first_turn_window(&prompt, 0.95);
    let active_config = EngineConfig {
        provider_connection: "mock".to_string(),
        model: "test-model".to_string(),
        temperature: None,
        context_length: Some(context_length),
        api_base_url: None,
        api_key: None,
        reasoning: None,
        compact_target_ratio: 0.5,
        soft_compaction_threshold: 0.7,
        hard_compaction_threshold: 0.9,
    };
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let active: Arc<dyn Engine> = Arc::new(EngineImpl::new(
        Arc::new(MockProvider::new(vec!["ok".to_string()], Duration::ZERO)),
        active_config.clone(),
    ));
    let compact_engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
        Arc::new(ScriptedSummaryProvider {
            recorded: Arc::clone(&recorded),
            fail: false,
        }),
        active_config,
    ));

    let (agent_loop, mut ui_rx) = AgentLoop::new(
        active,
        Arc::new(tools::default_registry(
            test_sandbox(),
            tools::ToolRegistryConfig::default(),
        )),
    );
    let mut agent_loop = agent_loop.with_compaction_engine(compact_engine);
    let handle = agent_loop.run();
    agent_loop
        .send_command(AgentCommand::Prompt(prompt.clone()))
        .await;
    let mut updates = collect_updates_until(&mut ui_rx, |update| {
        matches!(update, UiUpdate::Notice { text, .. }
                if text.contains("lossily"))
    })
    .await;
    settle_background_tasks().await;
    updates.extend(std::iter::from_fn(|| ui_rx.try_recv().ok()));

    assert!(
        recorded
            .lock()
            .expect("summary recording mutex is not poisoned")
            .is_empty(),
        "above the hard threshold the loop drops synchronously instead of waiting \
             for a background summary"
    );
    assert!(
        updates
            .iter()
            .any(|update| matches!(update, UiUpdate::Notice { text, .. }
                if text.contains("lossily"))),
        "the synchronous fallback is announced"
    );
    assert_eq!(
        updates
            .iter()
            .filter(|update| matches!(update, UiUpdate::StreamDone))
            .count(),
        1,
        "compaction does not start another conversation request"
    );
    drop(agent_loop);
    let context = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("loop returns")
        .expect("loop task ok")
        .expect("loop returns a context");

    assert!(context.compaction().is_none(), "no summary was produced");
    assert!(
        context.model_messages().iter().any(|message| matches!(
            message,
            Message::User { content, .. }
                if content.contains(compaction::recovery::REQUEST_OMISSION_MARKER)
        )),
        "the oversized task is shortened with disclosure, not removed"
    );
    let active_tools = compaction_test_tools();
    let active_engine = test_engine();
    let trimmed_usage = context.estimated_model_tokens(&active_tools, active_engine.as_ref())
        as f64
        / context_length as f64;
    assert!(
        (trimmed_usage - 0.5).abs() < 0.4,
        "the trimmed view is near the compaction target: {trimmed_usage}"
    );
    assert!(
        context.messages().iter().any(|message| matches!(
            message,
            Message::User { content, .. } if content.as_str() == prompt.as_str()
        )),
        "the transcript keeps every original message"
    );
}
