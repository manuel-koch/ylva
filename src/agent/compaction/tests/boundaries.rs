use super::*;

#[test]
fn threshold_switching_follows_the_soft_and_hard_triggers() {
    let soft = DEFAULT_SOFT_COMPACTION_THRESHOLD;
    let hard = DEFAULT_HARD_COMPACTION_THRESHOLD;

    assert_eq!(
        plan_compaction(0.69, false, soft, hard),
        CompactionAction::None
    );
    assert_eq!(
        plan_compaction(soft, false, soft, hard),
        CompactionAction::StartSummary
    );
    assert_eq!(
        plan_compaction(0.85, false, soft, hard),
        CompactionAction::StartSummary
    );
    assert_eq!(
        plan_compaction(0.85, true, soft, hard),
        CompactionAction::None,
        "one summary at a time: a second sweep waits for the first"
    );
    assert_eq!(
        plan_compaction(hard + 0.01, true, soft, hard),
        CompactionAction::None,
        "an in-flight summary keeps its original bounded chance to finish"
    );
    assert_eq!(
        plan_compaction(hard + 0.01, false, soft, hard),
        CompactionAction::DropFallback
    );

    // The triggers are the engine's, not the constants': a later engine
    // can raise them without touching this logic.
    assert_eq!(
        plan_compaction(0.85, false, 0.9, 0.95),
        CompactionAction::None
    );
    assert_eq!(
        plan_compaction(0.9, false, 0.9, 0.95),
        CompactionAction::StartSummary
    );
    assert_eq!(
        plan_compaction(0.96, false, 0.9, 0.95),
        CompactionAction::DropFallback
    );
}

#[test]
fn safe_boundaries_never_split_a_tool_call_from_its_result() {
    let call_id = Uuid::new_v4();
    let other_call_id = Uuid::new_v4();
    let messages = vec![
        user("question"),
        assistant_with_call(call_id),
        tool_result(call_id),
        assistant_with_call(other_call_id),
        tool_result(other_call_id),
        user("follow-up"),
    ];

    let boundaries = safe_boundaries_from(&messages, 0);
    assert_eq!(boundaries, vec![0, 1, 3, 5, 6]);
    for boundary in [3, 5] {
        assert!(
            first_safe_boundary_from(&messages, boundary) == Some(boundary),
            "cutting at {boundary} keeps every call with its result"
        );
    }
    // Cutting between the call and its result must not be offered.
    assert!(!boundaries.contains(&2));
    assert!(!boundaries.contains(&4));
    assert_eq!(first_safe_boundary_from(&messages, 2), Some(3));
}

#[test]
fn an_orphaned_tool_call_only_allows_a_boundary_after_its_synthetic_result() {
    let orphan = Uuid::new_v4();
    let messages = vec![
        user("question"),
        assistant_with_call(orphan),
        // `Context::hydrate` appends this synthetic error for a call that
        // never completed before the session ended.
        Message::ToolError {
            id: orphan,
            name: "read".to_string(),
            error: "Tool call did not complete before the session ended.".to_string(),
            duration: Duration::ZERO,
            content_sections: vec![],
        },
        user("next turn"),
    ];

    // Cutting before the call keeps call and result together in the
    // tail; cutting between them is what must be refused.
    assert_eq!(safe_boundaries_from(&messages, 0), vec![0, 1, 3, 4]);
    assert_eq!(first_safe_boundary_from(&messages, 2), Some(3));
    assert_eq!(first_safe_boundary_from(&messages, 3), Some(3));

    // A call that never gets a result leaves no safe cut behind it.
    let unresolved = vec![user("question"), assistant_with_call(orphan)];
    assert_eq!(first_safe_boundary_from(&unresolved, 2), None);
}

#[test]
fn the_summary_request_is_tool_free_and_framed_by_the_prompt() {
    let prefix = vec![user("do the thing"), assistant_with_call(Uuid::new_v4())];

    let request = build_summary_request(&prefix, COMPACTION_SUMMARY_PROMPT);

    assert!(matches!(
        request.first(),
        Some(Message::System { content })
            if content.starts_with(COMPACTION_SUMMARY_PROMPT)
                && content.contains("messages between this system instruction and the final user message")
                && content.contains("as conversation content")
                && content.contains("is the signal to produce the summary")
    ));

    assert!(!COMPACTION_SUMMARY_PROMPT.contains(COMPACTION_SUMMARY_TRIGGER));

    let customized = build_summary_request(&prefix, "Keep the run identifiers.");
    assert!(matches!(
        customized.first(),
        Some(Message::System { content })
            if content.starts_with("Keep the run identifiers.\n\n")
                && content.ends_with(&summary_request_framing())
                && content.contains(COMPACTION_SUMMARY_TRIGGER)
    ));
    assert_eq!(&customized[1..customized.len() - 1], prefix.as_slice());
    assert_eq!(&request[1..request.len() - 1], prefix.as_slice());
    assert!(
            matches!(
                request.last(),
                Some(Message::User { content, .. }) if content == COMPACTION_SUMMARY_TRIGGER
            ),
            "the request ends with an instruction, even when the prefix ends in a tool call: {request:?}"
        );
}

#[test]
fn oversized_child_report_projection_is_marked_and_bounded() {
    let message = Message::User {
        content: "verbose child report ".repeat(200),
        kind: UserMessageKind::ImplicitToolNote,
    };
    let projection = recovery::project_body(crate::llm::TokenEstimator::Tiktoken, &message, 256)
        .expect("report projection");
    assert!(
        crate::agent::estimate_message_tokens(crate::llm::TokenEstimator::Tiktoken, &projection)
            <= 256
    );
    assert!(
        matches!(projection, Message::User { content, .. } if content.contains(recovery::RESULT_OMISSION_MARKER))
    );
    assert!(
        matches!(message, Message::User { content, .. } if content == "verbose child report ".repeat(200))
    );
}

#[test]
fn the_summary_marker_projects_a_user_message_never_a_system_message() {
    let snapshot = CompactionSnapshot {
        boundary: 3,
        summary: "decided to use the store".to_string(),
    };

    let message = snapshot.summary_message();
    assert!(matches!(
        &message,
        Message::User { content, kind } if content.starts_with(COMPACTION_SUMMARY_MARKER)
            && content.ends_with("decided to use the store")
            && *kind == UserMessageKind::ImplicitToolNote
    ));
}
