use super::*;

#[test]
fn the_budget_combines_the_window_with_the_engines_compaction_knobs() {
    let config = crate::llm::EngineConfig {
        provider_connection: "mock".to_string(),
        model: "test-model".to_string(),
        temperature: None,
        context_length: None,
        api_base_url: None,
        api_key: None,
        reasoning: None,
        compact_target_ratio: 0.4,
        soft_compaction_threshold: 0.8,
        hard_compaction_threshold: 0.95,
    };

    let budget = CompactionBudget::new(10_000, &config).expect("a real window yields a budget");
    assert_eq!(budget.context_length, 10_000);
    assert_eq!(budget.target_ratio, 0.4);
    assert_eq!(budget.soft_threshold, 0.8);
    assert_eq!(budget.hard_threshold, 0.95);
    assert_eq!(budget.target_tokens(), 4_000);

    assert_eq!(
        CompactionBudget::new(0, &config),
        None,
        "a zero-length window has no budget: the sweep must not divide by it"
    );
}

#[test]
fn summary_timeout_comes_from_the_engine_generating_the_summary() {
    let timeout = Duration::from_secs(240);
    let compact_engine = Some(Arc::new(crate::llm::EngineImpl::new(
        Arc::new(crate::llm::MockProvider::new(vec![], Duration::ZERO)),
        crate::llm::EngineConfig {
            provider_connection: "mock".to_string(),
            model: "compact-model".to_string(),
            temperature: None,
            context_length: None,
            api_base_url: None,
            api_key: None,
            reasoning: None,
            compact_target_ratio: 0.5,
            soft_compaction_threshold: 0.7,
            hard_compaction_threshold: 0.9,
        },
    )) as Arc<dyn Engine>);
    let (runtime, _outcome_rx) = CompactionRuntime::new(
        compact_engine,
        COMPACTION_SUMMARY_PROMPT.to_string(),
        timeout,
    );

    assert_eq!(runtime.summary_timeout, Duration::from_secs(240));
}

#[test]
fn effectiveness_reports_net_token_savings_and_message_reduction() {
    let summary_shrunk_view = CompactionEffectiveness::measure(1_000, 400, 12, 5);
    assert_eq!(summary_shrunk_view.estimated_token_savings, 600);
    assert_eq!(summary_shrunk_view.estimated_reduction_percent, 60.0);
    assert_eq!(summary_shrunk_view.removed_model_view_messages(), 7);
    assert_eq!(summary_shrunk_view.estimated_tokens_before, 1_000);
    assert_eq!(summary_shrunk_view.estimated_tokens_after, 400);

    let large_summary = CompactionEffectiveness::measure(100, 140, 3, 2);
    assert_eq!(large_summary.estimated_token_savings, -40);
    assert_eq!(large_summary.estimated_reduction_percent, -40.0);
    assert_eq!(large_summary.removed_model_view_messages(), 1);

    let empty_view = CompactionEffectiveness::measure(0, 0, 0, 0);
    assert_eq!(empty_view.estimated_reduction_percent, 0.0);
    assert_eq!(empty_view.removed_model_view_messages(), 0);
}

#[tokio::test]
async fn capacity_warning_uses_summarizer_window_and_counts_prompt_overhead() {
    let (runtime, _, _, _, _, _) = new_recording_runtime("A summary.");
    let overhead = summary_request_tokens(&build_summary_request(&[], COMPACTION_SUMMARY_PROMPT));
    for summary_context_length in [14_000 + overhead, 14_000 + overhead - 1] {
        let (boundary_runtime, _) = CompactionRuntime::new(
            Some(capacity_test_engine(Some(summary_context_length))),
            COMPACTION_SUMMARY_PROMPT.to_string(),
            Duration::from_secs(1),
        );
        assert_eq!(
            boundary_runtime
                .capacity_warning(capacity_test_engine(Some(20_000)))
                .await
                .is_some(),
            summary_context_length < 14_000 + overhead,
            "prompt and request overhead must decide the exact capacity boundary",
        );
    }
    let summary_engine = runtime.engine.as_ref().unwrap();
    let conversation_engine = capacity_test_engine(Some(20_000));
    summary_engine.adopt_catalog_context_length(Some(100_000));
    assert!(runtime
        .capacity_warning(conversation_engine)
        .await
        .is_none());

    let conversation_engine = capacity_test_engine(Some(24_000));
    let warning = runtime
        .capacity_warning(conversation_engine.clone())
        .await
        .expect("soft trigger plus overhead exceeds the summarizer's explicit window");
    assert!(warning.contains("16800 tokens (24000"));
    assert!(warning.contains("16000 tokens"));
    assert!(warning.contains(&format!("{overhead} tokens")));
    assert!(runtime
        .capacity_warning(conversation_engine)
        .await
        .is_none());

    assert!(runtime
        .capacity_warning(capacity_test_engine(Some(10_000)))
        .await
        .is_none());
    assert!(
        runtime
            .capacity_warning(capacity_test_engine(Some(24_000)))
            .await
            .is_some(),
        "returning to an unsafe configuration warns again"
    );
}

#[tokio::test]
async fn capacity_warning_follows_catalog_windows_and_conversation_switches() {
    let summary_engine = capacity_test_engine(None);
    summary_engine.adopt_catalog_context_length(Some(8_000));
    let (runtime, _) = CompactionRuntime::new(
        Some(summary_engine.clone()),
        COMPACTION_SUMMARY_PROMPT.to_string(),
        Duration::from_secs(1),
    );
    let conversation_engine = capacity_test_engine(None);
    conversation_engine.adopt_catalog_context_length(Some(20_000));
    let warning = runtime
        .capacity_warning(conversation_engine.clone())
        .await
        .expect("catalog capacities are used");
    assert!(warning.contains("14000 tokens (20000"));
    assert!(warning.contains("8000 tokens"));
    conversation_engine.adopt_catalog_context_length(Some(40_000));
    let warning = runtime
        .capacity_warning(conversation_engine.clone())
        .await
        .expect("changed conversation window is checked again");
    assert!(warning.contains("28000 tokens (40000"));
    assert!(warning.contains("8000 tokens"));
    summary_engine.adopt_catalog_context_length(Some(40_000));
    assert!(runtime
        .capacity_warning(conversation_engine)
        .await
        .is_none());
}

#[tokio::test]
async fn disabled_compaction_has_no_capacity_warning() {
    let (runtime, _) = CompactionRuntime::new(None, String::new(), Duration::from_secs(1));
    assert!(runtime
        .capacity_warning(capacity_test_engine(Some(100_000)))
        .await
        .is_none());
}

#[test]
fn summary_request_capacity_checks_exact_boundary_with_framing_and_overhead() {
    let prefix = vec![user("source ".repeat(500).as_str())];
    let request = build_summary_request(&prefix, &"guidelines ".repeat(100));
    let estimated_tokens = summary_request_tokens(&request);
    let prefix_tokens =
        crate::agent::estimate_message_tokens(crate::llm::TokenEstimator::Tiktoken, &prefix[0]);
    assert!(estimated_tokens > prefix_tokens + REQUEST_FIXED_OVERHEAD_TOKENS);
    assert!(summary_request_capacity_warning(
        &request,
        capacity_test_engine(Some(estimated_tokens)).as_ref(),
    )
    .is_none());
    let warning = summary_request_capacity_warning(
        &request,
        capacity_test_engine(Some(estimated_tokens - 1)).as_ref(),
    )
    .expect("one token above capacity warns");
    assert!(warning.contains(&format!("{estimated_tokens} tokens")));
    assert!(warning.contains("compaction prompt and request overhead"));
}
