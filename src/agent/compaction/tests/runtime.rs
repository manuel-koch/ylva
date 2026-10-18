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
async fn oversized_summary_requests_emit_root_and_subagent_notices_without_changing_source() {
    for is_subagent in [false, true] {
        let (mut runtime, mut outcomes, mut updates, run_id, _, context) =
            new_recording_runtime("A summary.");
        let run_id = if is_subagent { run_id } else { None };
        let (sender, receiver) = mpsc::channel(4);
        runtime.set_activity_sender(sender, run_id);
        updates.close();
        updates = receiver;
        runtime.summary_prompt = "long guidelines ".repeat(10_000);
        let transcript = context.messages().to_vec();
        runtime.start_summary(&context, 100_000, 50_000, 0, |context, _| {
            Some(context.messages().len())
        });
        let outcome = tokio::time::timeout(Duration::from_secs(1), outcomes.recv())
            .await
            .expect("summary finishes")
            .expect("summary outcome");
        assert!(matches!(outcome, CompactionOutcome::SummaryReady { .. }));
        let mut notices = Vec::new();
        while let Ok(update) = updates.try_recv() {
            if let crate::agent::UiUpdate::Notice {
                text,
                run_id: notice_run_id,
            } = update
            {
                assert_eq!(notice_run_id, run_id);
                notices.push(text);
            }
        }
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("this summary request needs approximately"));
        assert_eq!(context.messages(), transcript);
    }
}

#[tokio::test]
async fn schedule_summary_starts_only_above_the_soft_threshold_and_only_once() {
    let (mut runtime, _outcomes, _updates, _run_id, engine, mut context) =
        new_recording_runtime("A summary.");
    assert!(
        !runtime.schedule_summary(&context, engine.as_ref(), &[], 0),
        "a small view is below the soft threshold"
    );
    context.push_assistant("ok".to_string(), crate::agent::AssistantStatus::Complete);
    context.push_user("filler ".repeat(12_000));
    context.push_assistant("ok".to_string(), crate::agent::AssistantStatus::Complete);
    context.push_user("next".to_string());
    assert!(runtime.schedule_summary(&context, engine.as_ref(), &[], 0));
    assert!(
        !runtime.schedule_summary(&context, engine.as_ref(), &[], 0),
        "a summary already in flight is never duplicated"
    );
}

#[test]
fn inactive_request_gate_does_not_tokenize_tool_schemas() {
    let (mut runtime, _outcomes, _updates, _run_id, engine, mut context) =
        new_recording_runtime("A summary.");
    context.set_token_estimator(crate::llm::TokenEstimator::FourCharactersPerToken);
    let tools = vec![crate::llm::ToolSchema {
        name: "read".to_string(),
        description: "Read a file.".to_string(),
        parameters: serde_json::json!({"type": "object"}),
    }];
    let tokenizer = tiktoken_rs::cl100k_base_singleton();
    let tokenizer_lock = tokenizer.lock();
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let deferred = runtime.defer_request(&context, engine.as_ref(), &tools);
        sender.send(deferred).expect("send gate decision");
    });

    let result = receiver.recv_timeout(Duration::from_secs(1));
    drop(tokenizer_lock);
    worker.join().expect("request gate worker");
    assert!(
        !result.expect("an inactive gate must not wait for the tokenizer"),
        "only a running summary can defer a request"
    );
}

#[tokio::test(start_paused = true)]
async fn request_gate_uses_the_existing_deadline_and_ignores_stale_identities() {
    let (mut runtime, _outcomes, _updates, _run_id, engine, mut context) =
        new_recording_runtime("A summary.");
    context.push_user("task ".repeat(20_000));
    let identity = Uuid::new_v4();
    runtime.summary_in_flight = true;
    runtime.request_id = Some(identity);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    runtime.summary_deadline = Some(deadline);
    assert!(runtime.defer_request(&context, engine.as_ref(), &[]));
    tokio::time::advance(Duration::from_secs(3)).await;
    assert!(runtime.defer_request(&context, engine.as_ref(), &[]));
    assert_eq!(runtime.summary_deadline, Some(deadline));
    assert!(!runtime.on_outcome(
        CompactionOutcome::SummaryFailed {
            generation: 0,
            request_id: Some(Uuid::new_v4()),
            reason: "stale".to_string(),
        },
        0
    ));
    assert!(runtime.summary_in_flight);
    assert!(!runtime.take_waiting_continuation());
    assert!(runtime.on_outcome(
        CompactionOutcome::SummaryFailed {
            generation: 0,
            request_id: Some(identity),
            reason: "timeout".to_string(),
        },
        0
    ));
    assert!(runtime.take_waiting_continuation());
    assert!(!runtime.take_waiting_continuation());
    assert!(!runtime.on_outcome(
        CompactionOutcome::SummaryReady {
            generation: 0,
            request_id: Some(identity),
            boundary: context.messages().len(),
            summary: "late duplicate".to_string(),
        },
        0
    ));
    runtime.reset().await;
    assert!(!runtime.summary_pending());
    assert!(!runtime.continuation_waiting);
    assert!(runtime.summary_deadline.is_none());
}

#[tokio::test]
async fn scheduling_an_open_batch_summarizes_only_a_completed_prefix() {
    let (mut runtime, mut outcomes, _updates, _run_id, engine, mut context) =
        new_recording_runtime("A summary.");
    context.push_assistant("older evidence ".repeat(12_000), AssistantStatus::Complete);
    context.push_user("current task".to_string());
    let open_index = context.messages().len();
    context.push_assistant_tool_call(
        String::new(),
        AssistantStatus::Complete,
        vec![ToolCallRequest {
            id: Uuid::new_v4(),
            name: "web_fetch".to_string(),
            arguments: Value::Null,
            persisted_title_arguments: Vec::new(),
        }],
    );
    assert!(runtime.schedule_summary(&context, engine.as_ref(), &[], 0));
    let outcome = outcomes.recv().await.expect("summary");
    assert!(
        matches!(outcome, CompactionOutcome::SummaryReady { boundary, .. } if boundary <= open_index)
    );
}

#[tokio::test]
async fn compaction_activity_ends_immediately_on_failure() {
    let (mut runtime, mut outcomes, mut updates, run_id, _engine, context) =
        new_recording_runtime("");
    runtime.start_summary(&context, 16_000, 8_000, 0, |context, _| {
        Some(context.messages().len())
    });
    let started = tokio::time::timeout(Duration::from_secs(2), updates.recv())
        .await
        .expect("activity start deadline")
        .expect("activity start");
    let request_id = match started {
        crate::agent::UiUpdate::CompactionActivity {
            run_id: actual_run_id,
            request_id,
            active: true,
        } => {
            assert_eq!(actual_run_id, run_id);
            request_id
        }
        other => panic!("unexpected start update: {other:?}"),
    };
    let outcome = outcomes.recv().await.expect("summary outcome");
    assert!(matches!(outcome, CompactionOutcome::SummaryFailed { .. }));
    runtime.on_outcome(outcome, 0);
    // A failure can never be applied, so the indicator clears as soon as
    // `on_outcome` records it — no sweep needed.
    let finished = tokio::time::timeout(Duration::from_secs(2), updates.recv())
        .await
        .expect("activity completion deadline")
        .expect("activity completion");
    assert!(matches!(
        finished,
        crate::agent::UiUpdate::CompactionActivity {
            run_id: actual_run_id,
            request_id: actual_request_id,
            active: false,
        } if actual_run_id == run_id && actual_request_id == request_id
    ));
    assert!(!runtime.summary_pending());
}

#[tokio::test]
async fn reset_delivers_scoped_activity_end_even_when_the_ui_channel_is_full() {
    let (mut runtime, _) = CompactionRuntime::new(
        None,
        COMPACTION_SUMMARY_PROMPT.to_string(),
        Duration::from_secs(10),
    );
    let (sender, mut updates) = mpsc::channel(1);
    let request_id = Uuid::new_v4();
    let run_id = Some(Uuid::new_v4());
    sender
        .send(crate::agent::UiUpdate::CompactionActivity {
            run_id,
            request_id,
            active: true,
        })
        .await
        .expect("start fills channel");
    runtime.set_activity_sender(sender, run_id);
    runtime.request_id = Some(request_id);
    runtime.reset().await;
    assert!(runtime.request_id.is_none());
    assert!(matches!(
        updates.recv().await,
        Some(crate::agent::UiUpdate::CompactionActivity { active: true, .. })
    ));
    let ended = tokio::time::timeout(Duration::from_secs(1), updates.recv())
        .await
        .expect("end delivered");
    assert!(
        matches!(ended, Some(crate::agent::UiUpdate::CompactionActivity {
            run_id: actual_run_id, request_id: actual_request_id, active: false,
        }) if actual_run_id == run_id && actual_request_id == request_id)
    );
}

#[tokio::test]
async fn compaction_activity_stays_active_until_summary_is_applied() {
    let (mut runtime, mut outcomes, mut updates, run_id, engine, mut context) =
        new_recording_runtime("A summary.");
    runtime.start_summary(&context, 16_000, 8_000, 0, |context, _| {
        Some(context.messages().len())
    });
    let started = tokio::time::timeout(Duration::from_secs(2), updates.recv())
        .await
        .expect("activity start deadline")
        .expect("activity start");
    let request_id = match started {
        crate::agent::UiUpdate::CompactionActivity {
            run_id: actual_run_id,
            request_id,
            active: true,
        } => {
            assert_eq!(actual_run_id, run_id);
            request_id
        }
        other => panic!("unexpected start update: {other:?}"),
    };
    let outcome = outcomes.recv().await.expect("summary outcome");
    assert!(matches!(outcome, CompactionOutcome::SummaryReady { .. }));
    runtime.on_outcome(outcome, 0);
    assert!(runtime.summary_pending(), "outcome remains unapplied");
    // The summary is ready but not yet applied: the indicator must still
    // be active, so no completion update has been sent.
    assert!(
        updates.try_recv().is_err(),
        "indicator cleared before the summary was applied"
    );
    runtime.sweep(&mut context, engine.as_ref(), &[], 0);
    let finished = tokio::time::timeout(Duration::from_secs(2), updates.recv())
        .await
        .expect("activity completion deadline")
        .expect("activity completion");
    assert!(matches!(
        finished,
        crate::agent::UiUpdate::CompactionActivity {
            run_id: actual_run_id,
            request_id: actual_request_id,
            active: false,
        } if actual_run_id == run_id && actual_request_id == request_id
    ));
    assert!(!runtime.summary_pending());
}

#[test]
fn summary_application_log_reports_estimated_model_view_reduction() {
    let log = crate::test_support::capture_logs(async {
        let mut context = Context::new();
        context.set_token_estimator(crate::llm::TokenEstimator::FourCharactersPerToken);
        context.push_user("old request ".repeat(80));
        context.push_assistant("old response ".repeat(40), AssistantStatus::Complete);
        let boundary = context.messages().len();
        let context_length = 16_000;
        let config = crate::llm::EngineConfig {
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
        let engine: Arc<dyn Engine> = Arc::new(crate::llm::EngineImpl::new(
            Arc::new(crate::llm::MockProvider::new(vec![], Duration::ZERO)),
            config,
        ));
        let (mut runtime, _outcome_rx) = CompactionRuntime::new(
            Some(Arc::clone(&engine)),
            COMPACTION_SUMMARY_PROMPT.to_string(),
            Duration::from_secs(DEFAULT_COMPACTION_TIMEOUT_SECS),
        );
        runtime.pending = Some(CompactionSnapshot {
            boundary,
            summary: "short summary".to_string(),
        });

        let tokens_before = context.model_view_token_count();
        runtime.sweep(&mut context, engine.as_ref(), &[], 0);
        let tokens_after = context.model_view_token_count();
        assert!(tokens_after < tokens_before);
    });

    assert!(log.contains("compaction summary applied at a safe boundary"));
    assert!(log.contains("estimated_tokens_before="));
    assert!(log.contains("estimated_tokens_after="));
    assert!(log.contains("estimated_token_savings="));
    assert!(log.contains("estimated_reduction_percent="));
    assert!(log.contains("model_view_messages_removed="));
}

#[test]
fn synchronous_drop_log_reports_estimated_model_view_reduction() {
    let log = crate::test_support::capture_logs(async {
        let mut context = Context::new();
        context.set_token_estimator(crate::llm::TokenEstimator::FourCharactersPerToken);
        context.push_user("large old turn ".repeat(400));
        context.push_assistant("large old response ".repeat(200), AssistantStatus::Complete);
        let tokens_before = context.model_view_token_count();
        let config = crate::llm::EngineConfig {
            provider_connection: "mock".to_string(),
            model: "test-model".to_string(),
            temperature: None,
            context_length: Some(2_000),
            api_base_url: None,
            api_key: None,
            reasoning: None,
            compact_target_ratio: 0.5,
            soft_compaction_threshold: 0.7,
            hard_compaction_threshold: 0.9,
        };
        let engine: Arc<dyn Engine> = Arc::new(crate::llm::EngineImpl::new(
            Arc::new(crate::llm::MockProvider::new(vec![], Duration::ZERO)),
            config,
        ));
        let (mut runtime, _outcome_rx) = CompactionRuntime::new(
            Some(Arc::clone(&engine)),
            COMPACTION_SUMMARY_PROMPT.to_string(),
            Duration::from_secs(DEFAULT_COMPACTION_TIMEOUT_SECS),
        );

        runtime.sweep(&mut context, engine.as_ref(), &[], 0);
        assert!(context.model_view_token_count() < tokens_before);
    });

    assert!(log.contains("model view reached the hard compaction threshold"));
    assert!(log.contains("estimated_tokens_before="));
    assert!(log.contains("estimated_tokens_after="));
    assert!(log.contains("estimated_token_savings="));
    assert!(log.contains("estimated_reduction_percent="));
    assert!(log.contains("model_view_messages_removed="));
}

#[test]
fn no_op_sweep_log_reports_estimate_window_and_decision() {
    let log = crate::test_support::capture_logs(async {
        let mut context = Context::new();
        context.set_token_estimator(crate::llm::TokenEstimator::FourCharactersPerToken);
        context.push_user("small request".to_string());
        let config = crate::llm::EngineConfig {
            provider_connection: "mock".to_string(),
            model: "test-model".to_string(),
            temperature: None,
            context_length: Some(20_000),
            api_base_url: None,
            api_key: None,
            reasoning: None,
            compact_target_ratio: 0.5,
            soft_compaction_threshold: 0.7,
            hard_compaction_threshold: 0.9,
        };
        let engine: Arc<dyn Engine> = Arc::new(crate::llm::EngineImpl::new(
            Arc::new(crate::llm::MockProvider::new(vec![], Duration::ZERO)),
            config,
        ));
        let (mut runtime, _outcome_rx) = CompactionRuntime::new(
            Some(Arc::clone(&engine)),
            COMPACTION_SUMMARY_PROMPT.to_string(),
            Duration::from_secs(DEFAULT_COMPACTION_TIMEOUT_SECS),
        );

        runtime.sweep(&mut context, engine.as_ref(), &[], 0);
    });

    assert!(log.contains("compaction sweep evaluated"));
    assert!(log.contains("context_length=20000"));
    assert!(log.contains("estimated_tokens="));
    assert!(log.contains("usage_ratio="));
    assert!(log.contains("target_tokens=10000"));
    assert!(log.contains("soft_threshold=0.7"));
    assert!(log.contains("hard_threshold=0.9"));
    assert!(log.contains("action=None"));
}

#[test]
fn repeated_compaction_advances_and_stops_without_new_messages() {
    crate::test_support::run_locked(async {
        let mut context = Context::new();
        context.set_token_estimator(crate::llm::TokenEstimator::FourCharactersPerToken);
        context.push_user("old source ".repeat(2_000));
        context.push_assistant("old answer".to_string(), AssistantStatus::Complete);
        let previous_boundary = context.messages().len();
        assert!(context.set_compaction(CompactionSnapshot {
            boundary: previous_boundary,
            summary: "Earlier work.".to_string(),
        }));
        context.push_user("new source ".repeat(400));
        context.push_assistant("finished answer ".repeat(100), AssistantStatus::Complete);
        let transcript = context.messages().to_vec();
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let engine: Arc<dyn Engine> = Arc::new(crate::llm::EngineImpl::new(
            Arc::new(RecordingSummaryProvider {
                recorded: Arc::clone(&recorded),
                reply: "Updated work.".to_string(),
            }),
            crate::llm::EngineConfig {
                provider_connection: "mock".to_string(),
                model: "summary-model".to_string(),
                temperature: None,
                context_length: Some(context.model_view_token_count() * 3 / 2),
                api_base_url: None,
                api_key: None,
                reasoning: None,
                compact_target_ratio: 0.4,
                soft_compaction_threshold: 0.5,
                hard_compaction_threshold: 0.8,
            },
        ));
        let (mut runtime, mut outcome_rx) = CompactionRuntime::new(
            Some(Arc::clone(&engine)),
            COMPACTION_SUMMARY_PROMPT.to_string(),
            Duration::from_secs(1),
        );
        assert!(
            runtime
                .sweep(&mut context, engine.as_ref(), &[], 0)
                .summary_started
        );
        let outcome = tokio::time::timeout(Duration::from_secs(1), outcome_rx.recv())
            .await
            .expect("summary finishes")
            .expect("summary outcome");
        assert!(
            matches!(&outcome, CompactionOutcome::SummaryReady { boundary, .. } if *boundary > previous_boundary),
            "summary must cover new visible messages: {outcome:?}"
        );
        runtime.on_outcome(outcome, 0);
        let result = runtime.sweep(&mut context, engine.as_ref(), &[], 0);
        assert!(
            result.summary_applied,
            "the next summary must advance the boundary"
        );
        assert!(
            !result.summary_started,
            "unchanged context must not restart compaction"
        );
        assert!(!runtime.summary_pending());
        assert!(context.compaction().expect("snapshot").boundary > previous_boundary);
        assert_eq!(context.messages(), transcript);
        assert_eq!(recorded.lock().expect("recording mutex").len(), 1);
    });
}

#[tokio::test]
async fn generate_summary_collects_the_text_without_tools() {
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let engine: Arc<dyn Engine> = Arc::new(crate::llm::EngineImpl::new(
        Arc::new(RecordingSummaryProvider {
            recorded: Arc::clone(&recorded),
            reply: "  A concise summary.  ".to_string(),
        }),
        crate::llm::EngineConfig {
            provider_connection: "mock".to_string(),
            model: "summary-model".to_string(),
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
    let prefix = vec![user("do the thing")];

    let summary = generate_summary(
        engine,
        build_summary_request(&prefix, COMPACTION_SUMMARY_PROMPT),
        None,
        CancellationToken::new(),
        crate::http::RequestDumpFailureHandler::default(),
        None,
    )
    .await
    .expect("summary generated");

    assert_eq!(summary, "A concise summary.");
    let requests = recorded
        .lock()
        .expect("recording mutex is not poisoned")
        .clone();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert!(
        request.tools.is_empty(),
        "the summarizer is tool-free: {:?}",
        request.tools
    );
    assert_eq!(
        &request.messages[1..request.messages.len() - 1],
        prefix.as_slice()
    );
}

#[tokio::test]
async fn an_empty_summary_response_is_a_failure() {
    struct EmptyProvider;

    impl crate::llm::Provider for EmptyProvider {
        fn name(&self) -> &str {
            "empty"
        }

        fn list_models(
            &self,
        ) -> futures::future::BoxFuture<'_, anyhow::Result<Vec<crate::llm::ModelInfo>>> {
            Box::pin(async { Ok(vec![]) })
        }

        fn chat<'a>(
            &'a self,
            _messages: Vec<Message>,
            _tools: Vec<ToolSchema>,
            _cancel: CancellationToken,
        ) -> futures::future::BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
            Box::pin(async move {
                let (tx, rx) = mpsc::channel(4);
                tokio::spawn(async move {
                    let _ = tx.send(StreamEvent::Text("   \n".to_string())).await;
                    let _ = tx.send(StreamEvent::Done).await;
                });
                Ok(rx)
            })
        }
    }

    let engine: Arc<dyn Engine> = Arc::new(crate::llm::EngineImpl::new(
        Arc::new(EmptyProvider),
        crate::llm::EngineConfig {
            provider_connection: "mock".to_string(),
            model: "summary-model".to_string(),
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

    let error = generate_summary(
        engine,
        build_summary_request(&[user("hello")], COMPACTION_SUMMARY_PROMPT),
        None,
        CancellationToken::new(),
        crate::http::RequestDumpFailureHandler::default(),
        None,
    )
    .await
    .expect_err("an empty summary must fail so the fallback runs");
    assert!(error.to_string().contains("empty summary"), "{error}");
}

#[tokio::test]
async fn a_runaway_summary_fails_instead_of_buffering_without_bound() {
    struct RunawayProvider;

    impl crate::llm::Provider for RunawayProvider {
        fn name(&self) -> &str {
            "runaway"
        }

        fn list_models(
            &self,
        ) -> futures::future::BoxFuture<'_, anyhow::Result<Vec<crate::llm::ModelInfo>>> {
            Box::pin(async { Ok(vec![]) })
        }

        fn chat<'a>(
            &'a self,
            _messages: Vec<Message>,
            _tools: Vec<ToolSchema>,
            _cancel: CancellationToken,
        ) -> futures::future::BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
            Box::pin(async move {
                let (tx, rx) = mpsc::channel(4);
                tokio::spawn(async move {
                    // Endless until the consumer gives up and drops the receiver.
                    while tx
                        .send(StreamEvent::Text("s".repeat(64 * 1024)))
                        .await
                        .is_ok()
                    {}
                });
                Ok(rx)
            })
        }
    }

    let engine: Arc<dyn Engine> = Arc::new(crate::llm::EngineImpl::new(
        Arc::new(RunawayProvider),
        crate::llm::EngineConfig {
            provider_connection: "mock".to_string(),
            model: "summary-model".to_string(),
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

    let error = generate_summary(
        engine,
        build_summary_request(&[user("hello")], COMPACTION_SUMMARY_PROMPT),
        None,
        CancellationToken::new(),
        crate::http::RequestDumpFailureHandler::default(),
        None,
    )
    .await
    .expect_err("a runaway summary must fail so the fallback runs");
    assert!(error.to_string().contains("exceeded"), "{error}");
}

#[tokio::test]
async fn summary_timeout_records_failure_on_the_request_dump() {
    let handler = crate::http::RequestDumpFailureHandler::default();
    let recorded_failure = Arc::new(Mutex::new(None::<String>));
    let recorded_failure_for_callback = Arc::clone(&recorded_failure);
    handler.register(move |failure| {
        *recorded_failure_for_callback
            .lock()
            .expect("failure mutex is not poisoned") = Some(failure.to_string());
    });

    struct PendingProvider;
    impl crate::llm::Provider for PendingProvider {
        fn name(&self) -> &str {
            "pending"
        }
        fn list_models(
            &self,
        ) -> futures::future::BoxFuture<'_, anyhow::Result<Vec<crate::llm::ModelInfo>>> {
            Box::pin(async { Ok(vec![]) })
        }
        fn chat<'a>(
            &'a self,
            _messages: Vec<Message>,
            _tools: Vec<ToolSchema>,
            _cancel: CancellationToken,
        ) -> futures::future::BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
            Box::pin(async {
                let (sender, receiver) = mpsc::channel(1);
                tokio::spawn(async move {
                    std::future::pending::<()>().await;
                    drop(sender);
                });
                Ok(receiver)
            })
        }
    }

    let engine: Arc<dyn Engine> = Arc::new(crate::llm::EngineImpl::new(
        Arc::new(PendingProvider),
        crate::llm::EngineConfig {
            provider_connection: "mock".to_string(),
            model: "summary-model".to_string(),
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

    let outcome = generate_summary_attempt(
        engine,
        build_summary_request(&[user("hello")], COMPACTION_SUMMARY_PROMPT),
        None,
        3,
        1,
        Duration::from_millis(1),
        handler,
        None,
    )
    .await;

    assert!(
        matches!(outcome, CompactionOutcome::SummaryFailed { generation: 3, ref reason, .. } if reason.contains("timed out"))
    );
    assert_eq!(
        recorded_failure
            .lock()
            .expect("failure mutex is not poisoned")
            .as_deref(),
        Some("compaction summary timed out after 0 seconds")
    );
}
