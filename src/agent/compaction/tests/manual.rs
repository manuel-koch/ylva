use super::*;

#[test]
fn manual_scheduling_bypasses_both_threshold_and_target_without_restarting_duplicates() {
    crate::test_support::run_locked(async {
        let (mut runtime, mut outcomes, mut updates, _, engine, mut context) =
            new_recording_runtime("A short summary.");
        context.push_assistant("old answer ".repeat(500), AssistantStatus::Complete);
        let boundary = context.messages().len();
        context.push_user("retain this task".to_string());
        assert!(context.estimated_model_tokens(&[], engine.as_ref()) < 8_000);
        assert!(!runtime.schedule_summary(&context, engine.as_ref(), &[], 0));
        let ManualCompactionSchedule::Started { request_id } =
            runtime.schedule_manual_summary(&context, engine.as_ref(), 0)
        else {
            panic!("manual summary must start below target");
        };
        let deadline = runtime.summary_deadline;
        assert_eq!(
            runtime.schedule_manual_summary(&context, engine.as_ref(), 0),
            ManualCompactionSchedule::AlreadyPending {
                request_id: Some(request_id)
            }
        );
        assert_eq!(runtime.summary_deadline, deadline);
        let outcome = outcomes.recv().await.expect("summary outcome");
        assert!(matches!(
            &outcome,
            CompactionOutcome::SummaryReady {
                request_id: Some(identity), boundary: cut, ..
            } if *identity == request_id && *cut == boundary
        ));
        assert!(runtime.on_outcome(outcome, 0));
        assert_eq!(
            runtime.schedule_manual_summary(&context, engine.as_ref(), 0),
            ManualCompactionSchedule::AlreadyPending {
                request_id: Some(request_id)
            }
        );
        assert_eq!(runtime.summary_deadline, None);
        let canonical = context.messages().to_vec();
        let tokens_before = context.estimated_model_tokens(&[], engine.as_ref());
        context.push_implicit_user("concurrent tail".to_string());
        let result = runtime.sweep(&mut context, engine.as_ref(), &[], 0);
        assert!(result.summary_applied);
        assert!(!result.prefix_dropped);
        assert_eq!(
            result.manual_outcome,
            Some(ManualCompactionResult::Applied {
                request_id: Some(request_id)
            })
        );
        assert!(context.estimated_model_tokens(&[], engine.as_ref()) < tokens_before);
        assert_eq!(&context.messages()[..canonical.len()], canonical);
        assert!(matches!(
            context.model_messages().last(),
            Some(Message::User { content, .. }) if content == "concurrent tail"
        ));
        assert_eq!(
            runtime.schedule_manual_summary(&context, engine.as_ref(), 0),
            ManualCompactionSchedule::NothingEligible
        );
        let mut activity = Vec::new();
        while let Ok(update) = updates.try_recv() {
            if let crate::agent::UiUpdate::CompactionActivity { active, .. } = update {
                activity.push(active);
            }
        }
        assert_eq!(activity, vec![true, false]);
    });
}

#[test]
fn manual_failure_does_not_enable_lossy_fallback_and_reset_discards_late_outcomes() {
    crate::test_support::run_locked(async {
        let (mut runtime, mut outcomes, _, _, engine, mut context) = new_recording_runtime("   ");
        context.push_assistant(
            "prior completed answer".to_string(),
            AssistantStatus::Complete,
        );
        context.push_user("latest task".to_string());
        assert!(matches!(
            runtime.schedule_manual_summary(&context, engine.as_ref(), 5),
            ManualCompactionSchedule::Started { .. }
        ));
        let canonical = context.model_messages();
        let outcome = outcomes.recv().await.expect("empty outcome");
        assert!(matches!(
            &outcome,
            CompactionOutcome::SummaryFailed { reason, .. } if reason.contains("empty")
        ));
        assert!(runtime.on_outcome(outcome, 5));
        assert!(!runtime.fallback_due);
        assert!(!runtime.summary_pending());
        let result = runtime.sweep(&mut context, engine.as_ref(), &[], 5);
        assert_eq!(result, CompactionSweepResult::default());
        assert_eq!(context.model_messages(), canonical);
        let ManualCompactionSchedule::Started { request_id } =
            runtime.schedule_manual_summary(&context, engine.as_ref(), 5)
        else {
            panic!("new explicit attempt can start after failure");
        };
        runtime.reset().await;
        assert!(!runtime.on_outcome(
            CompactionOutcome::SummaryReady {
                generation: 5,
                request_id: Some(request_id),
                boundary: context.messages().len() - 1,
                summary: "late summary".to_string(),
            },
            5,
        ));
        assert_eq!(context.model_messages(), canonical);
    });
}

#[test]
fn manual_rejections_do_not_mutate_or_persist_even_under_hard_pressure() {
    crate::test_support::run_locked(async {
        let directory = tempfile::tempdir_in(".").expect("project-local fixture");
        let store = crate::store::Store::create(
            &directory.path().join("chats"),
            directory.path(),
            Uuid::new_v4(),
            "mock",
            "test-model",
            None,
        )
        .expect("store");
        let (mut runtime, _, _, _, engine, _) = new_recording_runtime("unused");
        let mut context = Context::new();
        context.set_token_estimator(crate::llm::TokenEstimator::FourCharactersPerToken);
        context.set_store(store.clone());
        context.push_user("old task ".repeat(20));
        context.push_assistant("old answer ".repeat(20), AssistantStatus::Complete);
        let boundary = context.messages().len();
        context.push_user("current task ".repeat(10_000));
        store.flush().await;
        let persisted_before = std::fs::read(store.path()).expect("initial transcript");
        let view_before = context.model_messages();
        // Equal estimates require rejection just as strictly as growth.
        let equal_summary = (0..500)
            .map(|length| "a".repeat(length))
            .find(|summary| {
                context.manual_compaction_estimate(
                    &CompactionSnapshot {
                        boundary,
                        summary: summary.clone(),
                    },
                    &[],
                ) == Some(context.estimated_model_tokens(&[], engine.as_ref()))
            })
            .expect("fixture must cover exactly equal token estimates");
        let summaries = [
            equal_summary,
            "growing summary ".repeat(2_000),
            String::new(),
        ];
        for summary in summaries {
            runtime.trigger = CompactionTrigger::Manual;
            runtime.pending = Some(CompactionSnapshot { boundary, summary });
            let result = runtime.sweep(&mut context, engine.as_ref(), &[], 0);
            assert!(!result.summary_applied);
            assert!(!result.prefix_dropped);
            assert!(!result.summary_started);
            assert!(result.manual_outcome.is_some());
            assert_eq!(context.model_messages(), view_before);
            assert!(context.compaction().is_none());
            context.persist_model_projection();
            store.flush().await;
            assert_eq!(
                std::fs::read(store.path()).expect("unchanged transcript"),
                persisted_before,
            );
        }
    });
}

#[test]
fn manual_unavailable_and_empty_context_outcomes_are_explicit() {
    let engine = crate::agent::tests::test_engine();
    let (mut runtime, _) =
        CompactionRuntime::new(None, "prompt".to_string(), Duration::from_secs(1));
    let mut context = Context::new();
    assert_eq!(
        runtime.schedule_manual_summary(&context, engine.as_ref(), 0),
        ManualCompactionSchedule::Unavailable
    );
    runtime.engine = Some(engine.clone());
    assert_eq!(
        runtime.schedule_manual_summary(&context, engine.as_ref(), 0),
        ManualCompactionSchedule::NothingEligible
    );
    context.push_user("one task".to_string());
    assert_eq!(
        runtime.schedule_manual_summary(&context, engine.as_ref(), 0),
        ManualCompactionSchedule::NothingEligible
    );
}

#[test]
fn manual_timeout_keeps_its_original_deadline_and_never_forces_fallback() {
    crate::test_support::run_locked_paused(async {
        let (mut runtime, mut outcomes, _, _, conversation_engine, mut context) =
            new_recording_runtime("unused");
        let configured_summary_engine: Arc<dyn Engine> = Arc::new(crate::llm::EngineImpl::new(
            Arc::new(crate::llm::MockProvider::new(
                vec!["first".to_string(), "delayed".to_string()],
                Duration::from_secs(10),
            )),
            conversation_engine.config(),
        ));
        runtime.engine = Some(configured_summary_engine);
        runtime.summary_timeout = Duration::from_secs(1);
        context.push_assistant(
            "completed earlier answer".to_string(),
            AssistantStatus::Complete,
        );
        context.push_user("latest task".to_string());
        let canonical = context.model_messages();
        runtime.schedule_manual_summary(&context, conversation_engine.as_ref(), 0);
        let deadline = runtime.summary_deadline;
        tokio::time::advance(Duration::from_millis(500)).await;
        assert!(matches!(
            runtime.schedule_manual_summary(&context, conversation_engine.as_ref(), 0),
            ManualCompactionSchedule::AlreadyPending { .. }
        ));
        assert_eq!(runtime.summary_deadline, deadline);
        let outcome = outcomes.recv().await.expect("timeout outcome");
        assert!(matches!(
            &outcome,
            CompactionOutcome::SummaryFailed { reason, .. } if reason.contains("timed out")
        ));
        assert!(runtime.on_outcome(outcome, 0));
        assert!(!runtime.fallback_due);
        assert_eq!(context.model_messages(), canonical);
    });
}
