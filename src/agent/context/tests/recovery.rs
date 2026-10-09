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
fn manual_boundary_retains_the_latest_explicit_task_and_never_repeats_a_summary() {
    let mut context = Context::new();
    assert_eq!(context.manual_summary_boundary(), None);
    context.push_user("first task".to_string());
    context.push_assistant("first answer".to_string(), AssistantStatus::Complete);
    assert_eq!(context.manual_summary_boundary(), None);
    let boundary = context.messages().len();
    context.push_user("latest task".to_string());
    context.push_implicit_user("not an explicit task".to_string());
    context.push_assistant("latest answer".to_string(), AssistantStatus::Complete);
    assert_eq!(context.manual_summary_boundary(), Some(boundary));
    assert!(context.set_compaction(compaction::CompactionSnapshot {
        boundary,
        summary: "earlier task".to_string(),
    }));
    assert_eq!(context.manual_summary_boundary(), None);
    context.push_user("another task".to_string());
    let next_boundary = context.manual_summary_boundary().expect("new source");
    assert!(next_boundary > boundary);
    let source = context.summary_source(next_boundary);
    assert_eq!(source[0], context.compaction().unwrap().summary_message());
    assert!(source.iter().any(|message| matches!(
        message, Message::User { content, kind: UserMessageKind::Explicit }
            if content == "latest task"
    )));
}

#[test]
fn manual_boundary_never_splits_an_open_tool_group_even_across_a_new_task() {
    let mut context = Context::new();
    context.push_user("first task".to_string());
    let safe_boundary = context.messages().len();
    let call_id = Uuid::new_v4();
    context.push_assistant_tool_call(
        String::new(),
        AssistantStatus::Complete,
        vec![ToolCallRequest {
            id: call_id,
            name: "read".to_string(),
            arguments: Value::Null,
            persisted_title_arguments: Vec::new(),
        }],
    );
    context.push_user("latest task".to_string());
    assert_eq!(context.manual_summary_boundary(), Some(safe_boundary));
    context.push_tool_error(
        call_id,
        "read".to_string(),
        "completed".to_string(),
        None,
        Duration::ZERO,
        vec![],
    );
    assert_eq!(context.manual_summary_boundary(), Some(safe_boundary));
    let complete_boundary = context.messages().len();
    context.push_user("new latest task".to_string());
    assert_eq!(context.manual_summary_boundary(), Some(complete_boundary));
}

#[test]
fn manual_candidate_estimate_is_pure_exact_and_rejects_unsafe_cuts() {
    let mut context = Context::new();
    context.push_user("old task ".repeat(200));
    context.push_assistant("old answer ".repeat(200), AssistantStatus::Complete);
    let boundary = context.messages().len();
    context.push_user("retained task".to_string());
    let snapshot = compaction::CompactionSnapshot {
        boundary,
        summary: "old work".to_string(),
    };
    let before = context.model_messages();
    let estimate = context
        .manual_compaction_estimate(&snapshot, &compaction_test_tools())
        .expect("valid cut");
    assert_eq!(context.model_messages(), before);
    assert!(context.compaction().is_none());
    assert!(!context.projection_dirty);
    context.push_implicit_user("a concurrently added tail".to_string());
    let estimate_with_tail = context
        .manual_compaction_estimate(&snapshot, &compaction_test_tools())
        .expect("tail additions do not invalidate a safe cut");
    assert!(estimate_with_tail > estimate);
    assert!(context.set_compaction(snapshot.clone()));
    assert_eq!(
        context.estimated_model_tokens(&compaction_test_tools(), test_engine().as_ref()),
        estimate_with_tail,
    );
    assert_eq!(context.manual_compaction_estimate(&snapshot, &[]), None);
    assert_eq!(
        context.manual_compaction_estimate(
            &compaction::CompactionSnapshot {
                boundary: context.messages().len(),
                summary: "unsafe".to_string(),
            },
            &[],
        ),
        None
    );
}

#[test]
fn recovery_budgets_markers_utf8_envelopes_and_a_shrunken_window() {
    let mut context = Context::new();
    context.push_user("retain the task".to_string());
    let id = Uuid::new_v4();
    context.push_assistant_tool_call(
        String::new(),
        AssistantStatus::Complete,
        vec![ToolCallRequest {
            id,
            name: "bash".to_string(),
            arguments: Value::Null,
            persisted_title_arguments: Vec::new(),
        }],
    );
    context.push_tool_error(
        id,
        "bash".to_string(),
        "失败 αβ \n\\\" ".repeat(4_000),
        Some(&"partial \nαβ ".repeat(4_000)),
        Duration::ZERO,
        Vec::new(),
    );
    let canonical = context.messages().to_vec();
    context.limit_last_model_message(256);
    let projection = context.model_messages().last().expect("error").clone();
    assert!(estimate_message_tokens(context.estimator, &projection) <= 256);
    assert!(
        matches!(projection, Message::ToolError { error, .. } if error.contains(compaction::recovery::RESULT_OMISSION_MARKER))
    );
    assert_eq!(context.messages(), canonical);
    assert!(context.available_result_token_budget(&[], 64_000) > 0);
    assert_eq!(context.available_result_token_budget(&[], 100), 0);
    let report = context.recover_model_view(2_000, 4_000, &[]);
    assert!(report.fits_ceiling);
    assert!(report.stage <= 5);
    assert_eq!(
        report.estimated_tokens_after,
        context.model_view_token_count()
    );
    let mut schemas = compaction_test_tools();
    schemas[0].description = "huge schema ".repeat(10_000);
    assert_eq!(context.available_result_token_budget(&schemas, 4_000), 0);
    assert!(
        !context
            .recover_model_view(2_000, 4_000, &schemas)
            .fits_ceiling
    );
}

#[test]
fn each_recovery_stage_is_measured_without_shortening_a_ceiling_safe_task() {
    let make_context = || {
        let mut context = Context::new();
        context.messages.clear();
        context.entries.clear();
        context.push_system("instructions".to_string());
        context.set_token_estimator(crate::llm::TokenEstimator::FourCharactersPerToken);
        context
    };
    let append_result = |context: &mut Context, text: String| {
        let id = Uuid::new_v4();
        context.push_assistant_tool_call(
            String::new(),
            AssistantStatus::Complete,
            vec![ToolCallRequest {
                id,
                name: "read".to_string(),
                arguments: Value::Null,
                persisted_title_arguments: Vec::new(),
            }],
        );
        context.push_tool_error(
            id,
            "read".to_string(),
            text,
            None,
            Duration::ZERO,
            Vec::new(),
        );
    };
    let mut unchanged = make_context();
    unchanged.push_user("task".to_string());
    assert_eq!(unchanged.recover_model_view(1_000, 2_000, &[]).stage, 0);

    let mut old_results = make_context();
    old_results.push_user("task".to_string());
    append_result(&mut old_results, "older output ".repeat(3_000));
    append_result(&mut old_results, "newest result".to_string());
    let report = old_results.recover_model_view(1_000, 2_000, &[]);
    assert_eq!(report.stage, 1);
    assert_eq!(report.clipped_results, 1);
    assert!(report.fits_target && report.fits_ceiling);

    let mut old_groups = make_context();
    old_groups.push_user("task".to_string());
    old_groups.push_assistant("older answer ".repeat(3_000), AssistantStatus::Complete);
    append_result(&mut old_groups, "newest result".to_string());
    let report = old_groups.recover_model_view(1_000, 2_000, &[]);
    assert_eq!(report.stage, 2);
    assert!(report.fits_target && report.fits_ceiling);
    assert!(old_groups.recovery.is_some());
    assert!(orphaned_tool_call_ids(&old_groups.model_messages()).is_empty());

    let mut retained_results = make_context();
    retained_results.push_user("task".to_string());
    append_result(&mut retained_results, "newest output ".repeat(3_000));
    let report = retained_results.recover_model_view(1_000, 2_000, &[]);
    assert_eq!(report.stage, 3);
    assert!(report.fits_ceiling);

    let mut summary = make_context();
    summary.push_user("earlier task".to_string());
    assert!(summary.set_compaction(compaction::CompactionSnapshot {
        boundary: summary.messages.len(),
        summary: "older summary ".repeat(3_000),
    }));
    let task = "retain this request ".repeat(100);
    summary.push_user(task.clone());
    let report = summary.recover_model_view(128, 2_000, &[]);
    assert_eq!(report.stage, 4);
    assert!(!report.fits_target && report.fits_ceiling);
    assert!(summary
        .summary_override
        .as_ref()
        .expect("shortened summary")
        .contains(compaction::recovery::SUMMARY_OMISSION_MARKER));
    assert!(summary.model_messages().iter().any(|message| matches!(
        message, Message::User { content, kind: UserMessageKind::Explicit } if content == &task
    )));

    let mut oversized_task = make_context();
    oversized_task.push_user("task ".repeat(4_000));
    let report = oversized_task.recover_model_view(1_000, 2_000, &[]);
    assert_eq!(report.stage, 5);
    assert!(report.fits_ceiling);
    assert_eq!(
        report.estimated_tokens_after,
        oversized_task.model_view_token_count()
    );
}

#[test]
fn an_oversized_new_task_shortens_the_newest_anchor_not_the_previous_one() {
    let mut context = Context::new();
    context.push_user("original task".to_string());
    context.push_assistant("older context ".repeat(5_000), AssistantStatus::Complete);
    context.recover_model_view(2_000, 4_000, &[]);
    context.push_user(format!("new task {}", "request ".repeat(10_000)));
    let report = context.recover_model_view(2_000, 4_000, &[]);
    assert!(report.fits_ceiling);
    assert!(context.model_messages().iter().any(|message| matches!(
            message, Message::User { content, kind: UserMessageKind::Explicit }
                if content.starts_with("new task") && content.contains(compaction::recovery::REQUEST_OMISSION_MARKER)
        )));
}

#[test]
fn set_compaction_replaces_the_prefix_and_only_moves_forward() {
    let mut context = Context::new();
    context.push_user("first turn".to_string());
    context.push_assistant("first answer".to_string(), AssistantStatus::Complete);
    context.push_user("second turn".to_string());
    let transcript_len = context.messages().len();

    assert!(
        context.set_compaction(compaction::CompactionSnapshot {
            boundary: 3,
            summary: "the first turn, summarized".to_string(),
        }),
        "the first snapshot applies"
    );

    let model_messages = context.model_messages();
    let leading_system_len = context.leading_system_len();
    assert_eq!(
        model_messages.len(),
        leading_system_len + 1 + (transcript_len - 3),
        "system block + the summary + the verbatim tail after the boundary"
    );
    assert!(
        matches!(
            &model_messages[leading_system_len],
            Message::User { content, .. }
                if content.starts_with(compaction::COMPACTION_SUMMARY_MARKER)
        ),
        "the summary is marked, not a system message: {model_messages:?}"
    );
    assert_eq!(
        &model_messages[leading_system_len + 1..],
        &context.messages()[3..],
        "the tail after the boundary is verbatim"
    );
    assert_eq!(
        context.messages().len(),
        transcript_len,
        "compaction never rewrites the transcript"
    );

    assert!(
        !context.set_compaction(compaction::CompactionSnapshot {
            boundary: 3,
            summary: "the same boundary again".to_string(),
        }),
        "an identical boundary must not supersede the applied snapshot"
    );
    assert!(
        !context.set_compaction(compaction::CompactionSnapshot {
            boundary: 2,
            summary: "an older boundary".to_string(),
        }),
        "boundaries only ever move forward"
    );
    assert_eq!(
        context.compaction().expect("snapshot kept").summary,
        "the first turn, summarized"
    );

    assert!(
        context.set_compaction(compaction::CompactionSnapshot {
            boundary: transcript_len,
            summary: "everything, summarized".to_string(),
        }),
        "a newer, larger boundary supersedes the applied one"
    );
    assert_eq!(
        context.model_messages().len(),
        leading_system_len + 2,
        "the whole prefix is represented by the summary and the preserved task anchor"
    );
}

#[test]
fn drop_model_view_to_cuts_only_at_complete_turn_boundaries() {
    let mut context = Context::new();
    context.set_token_estimator(crate::llm::TokenEstimator::FourCharactersPerToken);
    let call_id = Uuid::new_v4();
    context.push_user("do the work".to_string());
    context.push_assistant_tool_call(
        String::new(),
        AssistantStatus::Complete,
        vec![ToolCallRequest {
            id: call_id,
            name: "read".to_string(),
            arguments: Value::Null,
            persisted_title_arguments: Vec::new(),
        }],
    );
    context.push_tool_result(ToolResult {
        id: call_id,
        name: "read".to_string(),
        result: "x".repeat(8_000),
        attachments: vec![],
        result_section_id: None,
        result_metadata: None,
        result_metadata_schema: None,
        summary: vec![],
        content_sections: vec![],
        duration: Duration::ZERO,
    });
    context.push_user("and now this".to_string());

    let leading_system_len = context.leading_system_len();
    let system_tokens: usize = context.entries[..leading_system_len]
        .iter()
        .map(|entry| entry.token_count)
        .sum();
    let final_turn_tokens = context.entries[context.messages().len() - 1].token_count;
    // Room for the system prompts plus the final turn, but nowhere near
    // enough for the 8 000-character tool result that precedes it: the
    // only cut that fits must leave the result behind, never split it
    // from its call.
    let target_tokens = system_tokens + final_turn_tokens + 10;

    assert!(
        context.drop_model_view_to(target_tokens),
        "the target is reachable without the oversized result"
    );
    let model_messages = context.model_messages();
    assert_eq!(
        model_messages.len(),
        leading_system_len + 1,
        "the view keeps the system block and the final turn: {model_messages:?}"
    );
    assert_eq!(
        model_messages.last(),
        context.messages().last(),
        "the final turn stays verbatim"
    );
    assert_eq!(
        context.messages().len(),
        6,
        "the transcript keeps the dropped messages"
    );
    assert_eq!(
        context.model_view_token_count(),
        system_tokens + final_turn_tokens
    );

    assert!(
        !context.drop_model_view_to(1),
        "a target below the system prompts alone cannot be met"
    );
}

#[test]
fn summary_boundary_advances_past_a_synchronously_dropped_prefix() {
    let mut context = Context::new();
    context.set_token_estimator(crate::llm::TokenEstimator::FourCharactersPerToken);
    context.push_user("old source ".repeat(2_000));
    context.push_assistant("old answer".to_string(), AssistantStatus::Complete);
    let dropped_boundary = context.messages().len();
    context.push_user("new source ".repeat(100));
    assert!(context.drop_model_view_to(context.model_view_token_count_from(dropped_boundary)));
    let boundary = context
        .summary_boundary_for_target(context.model_view_token_count() - 10)
        .expect("visible tail can be summarized");
    assert!(boundary > dropped_boundary);
}

#[test]
fn summary_boundary_does_not_resummarize_an_empty_visible_tail() {
    let _lock = crate::test_support::SUBSCRIBER_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut context = Context::new();
    context.push_user("old source".to_string());
    assert!(context.set_compaction(compaction::CompactionSnapshot {
        boundary: context.messages().len(),
        summary: "large summary ".repeat(100),
    }));
    assert_eq!(context.summary_boundary_for_target(1), None);
}

#[test]
fn summary_boundary_for_target_never_cuts_a_tool_call_result_group() {
    let mut context = Context::new();
    context.set_token_estimator(crate::llm::TokenEstimator::FourCharactersPerToken);
    let call_id = Uuid::new_v4();
    context.push_user("do the work".to_string());
    context.push_assistant_tool_call(
        String::new(),
        AssistantStatus::Complete,
        vec![ToolCallRequest {
            id: call_id,
            name: "read".to_string(),
            arguments: Value::Null,
            persisted_title_arguments: Vec::new(),
        }],
    );
    context.push_tool_result(ToolResult {
        id: call_id,
        name: "read".to_string(),
        result: "y".repeat(8_000),
        attachments: vec![],
        result_section_id: None,
        result_metadata: None,
        result_metadata_schema: None,
        summary: vec![],
        content_sections: vec![],
        duration: Duration::ZERO,
    });
    context.push_user("follow-up".to_string());

    // A target that the raw accumulation would reach in the middle of the
    // tool-call/result pair (right after the assistant message).
    let target_tokens = context
        .model_view_token_count()
        .saturating_sub(context.token_count(2).expect("result entry") + 100);

    let boundary = context
        .summary_boundary_for_target(target_tokens)
        .expect("a prefix exists to summarize");
    assert!(
        compaction::first_safe_boundary_from(context.messages(), boundary) == Some(boundary),
        "the chosen boundary {boundary} must be a complete-turn cut"
    );
    assert!(
        boundary >= 4,
        "the cut has to include the whole tool round, not just its call: {boundary}"
    );
}

#[test]
fn recovery_after_a_large_tool_batch_keeps_the_task_and_pairs_results() {
    let mut context = Context::new();
    context.push_user("summarize the ten pages".to_string());
    let calls: Vec<ToolCallRequest> = (0..10)
        .map(|index| ToolCallRequest {
            id: Uuid::new_v4(),
            name: "web_fetch".to_string(),
            arguments: serde_json::json!({ "url": format!("https://example.com/{index}") }),
            persisted_title_arguments: Vec::new(),
        })
        .collect();
    context.push_assistant_tool_call(String::new(), AssistantStatus::Complete, calls.clone());
    for call in &calls {
        context.push_tool_result(ToolResult {
            id: call.id,
            name: call.name.clone(),
            result: "page text ".repeat(2_000),
            attachments: Vec::new(),
            result_section_id: None,
            result_metadata: None,
            result_metadata_schema: None,
            summary: Vec::new(),
            duration: std::time::Duration::ZERO,
            content_sections: Vec::new(),
        });
    }

    let report = context.recover_model_view(4_000, 6_000, &[]);

    assert!(report.changed);
    assert!(report.fits_ceiling);
    let view = context.model_messages();
    assert!(
        view.iter().any(|message| matches!(
            message,
            Message::User { content, .. } if content == "summarize the ten pages"
        )),
        "the task anchor survives recovery"
    );
    assert!(
        view.iter().any(|message| matches!(
            message,
            Message::User { content, .. }
                if content.contains(compaction::recovery::RECOVERY_HANDOFF_MARKER)
        )),
        "the handoff replaces the removed outcomes"
    );
    let mut open_calls = std::collections::HashSet::new();
    for message in &view {
        match message {
            Message::Assistant { tool_calls, .. } => {
                open_calls.extend(tool_calls.iter().map(|call| call.id));
            }
            Message::ToolResult { value } => assert!(
                open_calls.remove(&value.id),
                "tool result without its call in the view"
            ),
            _ => {}
        }
    }
    assert!(open_calls.is_empty(), "tool call without its result");
    assert_eq!(context.messages().len(), 14, "the transcript is untouched");
}

#[test]
fn recovery_of_a_view_that_already_fits_changes_nothing() {
    let mut context = Context::new();
    context.push_user("only task".to_string());
    let report = context.recover_model_view(100_000, 200_000, &[]);
    assert!(!report.changed);
    assert!(report.fits_target);
}
