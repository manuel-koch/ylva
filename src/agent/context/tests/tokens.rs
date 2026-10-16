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
fn message_and_tool_call_estimates_include_shared_framing_overhead() {
    let estimator = crate::llm::TokenEstimator::FourCharactersPerToken;
    let message = Message::User {
        content: String::new(),
        kind: UserMessageKind::Explicit,
    };
    assert_eq!(
        estimate_message_tokens(estimator, &message),
        TOKEN_FRAMING_OVERHEAD
    );

    let mut context = Context::with_system_prompt("");
    let tool_call = ToolCallRequest {
        id: Uuid::nil(),
        name: "read".into(),
        arguments: serde_json::Value::Null,
        persisted_title_arguments: Vec::new(),
    };
    let expected = estimator.estimate_tokens(&tool_call.name)
        + estimator.estimate_tokens(&tool_call.id.to_string())
        + estimator.estimate_tokens(&tool_call.arguments.to_string())
        + 2 * TOKEN_FRAMING_OVERHEAD;
    context.set_token_estimator(estimator);
    context.push_assistant_tool_call(String::new(), AssistantStatus::Complete, vec![tool_call]);
    assert_eq!(
        context.token_count(context.messages().len() - 1),
        Some(expected)
    );
}

#[test]
fn complete_delivery_checks_exact_envelope_including_escaped_metadata() {
    let id = Uuid::nil();
    let result = "Body \"quoted\"\nwith\\escapes";
    let metadata = Some(serde_json::json!({"source": "/a/\"b\"\nSKILL.md"}));
    let schema = Some(serde_json::json!({"description": "metadata ".repeat(100)}));
    let message = Message::ToolResult {
        value: ToolResult {
            id,
            name: "load_skill".into(),
            result: result.into(),
            attachments: vec![],
            result_section_id: None,
            result_metadata: metadata.clone(),
            result_metadata_schema: schema.clone(),
            summary: vec![],
            content_sections: vec![],
            duration: Duration::ZERO,
        },
    };
    let exact = estimate_message_tokens(crate::llm::TokenEstimator::Tiktoken, &message);
    assert!(complete_result_fits(
        id,
        "load_skill",
        result,
        metadata.clone(),
        schema.clone(),
        exact
    ));
    assert!(!complete_result_fits(
        id,
        "load_skill",
        result,
        metadata.clone(),
        schema,
        exact - 1
    ));
    assert!(complete_result_fits(
        id,
        "load_skill",
        result,
        None,
        None,
        exact - 1
    ));
}
#[test]
fn token_counts_are_cached_when_messages_enter_the_context() {
    let estimator = crate::llm::TokenEstimator::FourCharactersPerToken;
    let mut context = Context::new();
    context.set_token_estimator(estimator);
    context.push_user("hello world".to_string());
    context.push_assistant("hi".to_string(), AssistantStatus::Complete);
    context.push_tool_result(ToolResult {
        id: Uuid::new_v4(),
        name: "read".to_string(),
        result: "abcdefgh".to_string(),
        attachments: vec![],
        result_section_id: None,
        result_metadata: None,
        result_metadata_schema: None,
        summary: vec![],
        content_sections: vec![],
        duration: Duration::ZERO,
    });

    for index in 0..context.messages().len() {
        let expected = estimate_message_tokens(estimator, &context.messages()[index]);
        assert_eq!(
            context.token_count(index),
            Some(expected),
            "the count is cached at insertion for message {index}"
        );
    }
    let sum: usize = (0..context.messages().len())
        .map(|index| context.token_count(index).expect("entry exists"))
        .sum();
    assert_eq!(context.approximate_token_count(), sum);
    assert!(
        context.approximate_token_count() > 0,
        "a non-empty context occupies some tokens"
    );
    assert!(context.tier(0).is_some());
    assert_eq!(context.token_count(context.messages().len()), None);
}

#[test]
fn switching_the_token_estimator_refreshes_every_cached_count() {
    let tiktoken = crate::llm::TokenEstimator::Tiktoken;
    let heuristic = crate::llm::TokenEstimator::FourCharactersPerToken;
    let mut context = Context::new();
    assert_eq!(context.token_estimator(), tiktoken);
    context.push_user("plenty of words to estimate in this turn".to_string());
    for index in 0..context.messages().len() {
        assert_eq!(
            context.token_count(index),
            Some(estimate_message_tokens(
                tiktoken,
                &context.messages()[index]
            )),
            "counts start out under the default estimator"
        );
    }

    context.set_token_estimator(heuristic);
    for index in 0..context.messages().len() {
        assert_eq!(
            context.token_count(index),
            Some(estimate_message_tokens(
                heuristic,
                &context.messages()[index]
            )),
            "every cached count is recounted under the new estimator"
        );
    }

    let after_switch = context.approximate_token_count();
    context.set_token_estimator(heuristic);
    assert_eq!(
        context.approximate_token_count(),
        after_switch,
        "re-selecting the active estimator is a no-op"
    );
}

#[test]
fn context_tiers_order_identity_instructions_and_conversation() {
    let mut context = Context::new();
    assert_eq!(context.tier(0), Some(MessageTier::System));
    assert_eq!(context.tier(1), Some(MessageTier::InitialContext));

    context.push_user("first question".to_string());
    assert_eq!(context.tier(2), Some(MessageTier::Recent));

    // Instructions injected later (a child briefing, the iteration
    // budget) stay stable `InitialContext` wherever they land.
    context.push_system("work within the sandbox".to_string());
    assert_eq!(context.tier(3), Some(MessageTier::InitialContext));
    assert_eq!(context.tier(4), None);

    assert_eq!(
        context.model_messages(),
        context.messages(),
        "without a compaction snapshot the model view is the whole transcript"
    );
}

#[test]
fn hydration_populates_tiers_and_counts_for_replayed_messages() {
    let mut context = Context::new();
    context.hydrate(vec![
        Message::User {
            content: "a stored turn".to_string(),
            kind: UserMessageKind::Explicit,
        },
        Message::System {
            content: "stored instructions".to_string(),
        },
    ]);

    assert_eq!(context.tier(2), Some(MessageTier::Recent));
    assert_eq!(context.tier(3), Some(MessageTier::InitialContext));
    for index in 0..context.messages().len() {
        let expected =
            estimate_message_tokens(context.token_estimator(), &context.messages()[index]);
        assert_eq!(
            context.token_count(index),
            Some(expected),
            "replayed messages are counted too, for message {index}"
        );
    }
    assert!(context.approximate_token_count() > 0);
}

#[test]
fn estimated_usage_ratio_uses_the_active_engines_effective_context_length() {
    let mut context = Context::new();
    context.set_token_estimator(crate::llm::TokenEstimator::FourCharactersPerToken);
    context.push_user("0123456789abcdef".to_string());

    let configured = EngineConfig {
        provider_connection: "ollama".to_string(),
        model: "test-model".to_string(),
        temperature: None,
        context_length: Some(64),
        api_base_url: None,
        api_key: None,
        reasoning: None,

        compact_target_ratio: 0.5,
        soft_compaction_threshold: 0.7,
        hard_compaction_threshold: 0.9,
    };
    assert_eq!(configured.effective_context_length(None), 64);
    let unconfigured = EngineConfig {
        context_length: None,

        compact_target_ratio: 0.5,
        soft_compaction_threshold: 0.7,
        hard_compaction_threshold: 0.9,
        ..configured.clone()
    };
    assert_eq!(
        unconfigured.effective_context_length(Some(1024)),
        1024,
        "the provider catalog supplies the length when config does not"
    );
    assert_eq!(
        unconfigured.effective_context_length(None),
        crate::llm::DEFAULT_CONTEXT_LENGTH,
        "the built-in default is the last resort"
    );

    let tokens = context.approximate_token_count();
    let ratio = context.estimated_usage_ratio(configured.effective_context_length(None));
    assert!(
        (ratio - tokens as f64 / 64.0).abs() < f64::EPSILON,
        "usage is measured against the effective window: {ratio}"
    );
    assert_eq!(context.estimated_usage_ratio(0), 0.0);
}

#[test]
fn tool_schema_budget_reuses_counts_without_retokenizing() {
    let mut context = Context::new();
    context.set_token_estimator(crate::llm::TokenEstimator::FourCharactersPerToken);
    context.push_user("A bounded task.".to_string());
    let tools = compaction_test_tools();
    let expected_budget = context.available_result_token_budget(&tools, 64_000);
    let expected_tokens = context.model_view_token_count() + tool_schema_token_count(&tools);
    let tokenizer = tiktoken_rs::cl100k_base_singleton();
    let tokenizer_lock = tokenizer.lock();
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let engine = test_engine();
        assert_eq!(
            context.estimated_model_tokens(&tools, engine.as_ref()),
            expected_tokens
        );
        assert!(context
            .summary_boundary_for_model_target(64_000, &tools, engine.as_ref())
            .is_none());
        assert!(
            context
                .recover_model_view(64_000, 64_000, &tools)
                .fits_ceiling
        );
        let budget = context.available_result_token_budget(&tools, 64_000);
        sender.send(budget).expect("send budget");
    });

    let result = receiver.recv_timeout(Duration::from_secs(1));
    drop(tokenizer_lock);
    worker.join().expect("budget worker");
    assert_eq!(
        result.expect("unchanged schemas must not wait for the tokenizer"),
        expected_budget
    );
}

#[test]
fn tool_schema_budget_refreshes_when_definitions_change() {
    let mut context = Context::new();
    context.set_token_estimator(crate::llm::TokenEstimator::FourCharactersPerToken);
    let engine = test_engine();
    let mut tools = vec![ToolSchema {
        name: "read".to_string(),
        description: "Read a file.".to_string(),
        parameters: serde_json::json!({"type": "object"}),
    }];
    let check_estimate = |tools: &[ToolSchema]| {
        let expected_tokens = context.model_view_token_count() + tool_schema_token_count(tools);
        assert_eq!(
            context.estimated_model_tokens(tools, engine.as_ref()),
            expected_tokens
        );
        assert_eq!(
            context.available_result_token_budget(tools, 64_000),
            compaction::safe_request_ceiling(64_000).saturating_sub(expected_tokens)
        );
    };

    check_estimate(&tools);
    tools[0].name = "a different tool name ".repeat(10);
    check_estimate(&tools);
    tools[0].description = "A changed description with more details. ".repeat(20);
    check_estimate(&tools);
    tools[0].parameters = serde_json::json!({
        "type": "object",
        "properties": {"path": {"type": "string", "description": "A file path."}},
    });
    check_estimate(&tools);
    let parameters = tools[0].parameters.as_object_mut().expect("parameters");
    let parameter_type = parameters.shift_remove("type").expect("type");
    parameters.insert("type".to_string(), parameter_type);
    check_estimate(&tools);
    assert_eq!(
        context
            .tool_schema_token_cache
            .lock()
            .expect("tool schema cache")
            .inputs[2],
        tools[0].parameters.to_string(),
        "JSON field order is part of the tokenizer input"
    );
    tools.push(tools[0].clone());
    check_estimate(&tools);
    tools.remove(0);
    check_estimate(&tools);
    check_estimate(&[]);
    check_estimate(&tools);
}

#[test]
fn cached_tool_schema_budget_tracks_message_and_window_changes() {
    let mut context = Context::new();
    let tools = compaction_test_tools();
    let initial_budget = context.available_result_token_budget(&tools, 64_000);
    context.push_user("Additional context ".repeat(100));
    let updated_budget = context.available_result_token_budget(&tools, 64_000);
    assert!(updated_budget < initial_budget);
    let smaller_window = 32_000;
    assert_eq!(
        context.available_result_token_budget(&tools, smaller_window),
        compaction::safe_request_ceiling(smaller_window)
            .saturating_sub(context.model_view_token_count() + tool_schema_token_count(&tools))
    );
    context.set_token_estimator(crate::llm::TokenEstimator::FourCharactersPerToken);
    assert_eq!(
        context.available_result_token_budget(&tools, smaller_window),
        compaction::safe_request_ceiling(smaller_window)
            .saturating_sub(context.model_view_token_count() + tool_schema_token_count(&tools))
    );
}
