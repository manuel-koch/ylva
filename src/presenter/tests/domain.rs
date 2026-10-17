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
fn route_domain_logs_event_kind_and_mode_transition_without_chunk_text() {
    let log = crate::test_support::capture_logs(async {
        prime_domain_logging();
        let mut state = AppState::new();
        state.mode = TuiMode::Streaming;
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::StreamChunk(
                "super-secret-assistant-text".to_string(),
            )),
            &empty_textarea(),
        );
    });
    assert!(
        log.contains("domain event handled"),
        "route_domain must log a state-transition event: {log}"
    );
    assert!(
        log.contains("event=\"StreamChunk\""),
        "the event's variant tag must be logged: {log}"
    );
    assert!(
        log.contains("mode_before=\"Streaming\"") && log.contains("mode_after=\"Streaming\""),
        "the mode transition must be logged: {log}"
    );
    assert!(
        !log.contains("super-secret-assistant-text"),
        "chunk payload text must never be logged: {log}"
    );
}

#[test]
fn route_domain_logs_correlation_id_for_tool_events() {
    let id = Uuid::new_v4();
    let log = crate::test_support::capture_logs(async {
        prime_domain_logging();
        let mut state = AppState::new();
        state.mode = TuiMode::Streaming;
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolStart {
                id,
                name: "read".to_string(),
                arguments: serde_json::json!({"path": "/secret/path"}),
                content_sections: vec![],
                title_arguments: vec![],
                run_id: None,
            }),
            &empty_textarea(),
        );
    });
    assert!(
        log.contains(&format!("correlation_id=Some({id})")),
        "the tool call's id must be logged for correlation: {log}"
    );
    assert!(
        !log.contains("/secret/path"),
        "tool arguments must never be logged: {log}"
    );
}

#[test]
fn subagent_started_does_not_auto_switch_the_active_view() {
    // Spawning a subagent must not yank focus away from whatever the user
    // was looking at — only an *interactive pause* (ToolPending /
    // ClarifyPending) belonging to the subagent does that.
    let mut state = AppState::new();
    let run_id = Uuid::new_v4();
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::SubagentStarted {
            run_id,
            call_id: Uuid::new_v4(),
            agent_type: "reviewer".to_string(),
            task: "look into flaky test".to_string(),
            label: "Investigate flaky test".to_string(),
        }),
        &empty_textarea(),
    );
    assert_eq!(state.active_view, ConversationView::Root);
    assert!(!state.auto_switched_to_subagent);
}

#[test]
fn cancelled_subagent_releases_its_pending_tools_and_resumes_the_next_approval() {
    let mut state = AppState::new();
    let first_run_id = Uuid::new_v4();
    let second_run_id = Uuid::new_v4();
    for (run_id, task) in [(first_run_id, "first task"), (second_run_id, "second task")] {
        state.start_subagent_run(crate::tui_reducer_state::SubagentRunInfo {
            run_id,
            bubble_id: Uuid::new_v4(),
            agent_type: "reviewer".to_string(),
            task: task.to_string(),
            label: task.to_string(),
        });
    }
    let first_tool_id = Uuid::new_v4();
    let second_tool_id = Uuid::new_v4();
    for (run_id, tool_id) in [
        (first_run_id, first_tool_id),
        (second_run_id, second_tool_id),
    ] {
        state
            .subagent_conversations
            .get_mut(&run_id)
            .unwrap()
            .messages
            .push(ChatEntry::ToolBubble {
                id: tool_id,
                name: "bash".to_string(),
                state: Box::new(ToolCallState::Pending {
                    arguments: serde_json::json!({"command": "echo ready"}),
                    content_sections: vec![],
                    title_arguments: vec![],
                }),
                collapsed: true,
            });
        state.pending_approval_order.push(tool_id);
    }
    state.focus = Focus::ToolApproval {
        tool_id: first_tool_id,
        tool_name: "bash".to_string(),
    };
    state.note_interactive_pause(Some(first_run_id));

    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::SubagentDone {
            run_id: first_run_id,
            status: "cancelled".to_string(),
            result: None,
        }),
        &empty_textarea(),
    );

    assert!(matches!(
        state.subagent_conversations[&first_run_id].messages.first(),
        Some(ChatEntry::ToolBubble { state, .. }) if matches!(state.as_ref(), ToolCallState::Killed)
    ));
    assert!(!state.running_subagents.contains_key(&first_run_id));
    assert!(state.running_subagents.contains_key(&second_run_id));
    assert_eq!(state.pending_approval_order, vec![second_tool_id]);
    assert!(matches!(
        state.focus,
        Focus::ToolApproval { tool_id, .. } if tool_id == second_tool_id
    ));
    assert!(matches!(
        state.active_view,
        ConversationView::Subagent { run_id, .. } if run_id == second_run_id
    ));
}

#[test]
fn subagent_started_clears_transcript_without_seeding_a_synthetic_task_message() {
    // The task is shown via the real root `spawn_subagent` bubble
    // re-rendered as the subagent-view header (see
    // `render::project_for_draw`'s `subagent_view_bubble`) — the transcript
    // itself starts empty and only accumulates the subagent's own turns.
    let mut state = AppState::new();
    let run_id = Uuid::new_v4();
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::SubagentStarted {
            run_id,
            call_id: Uuid::new_v4(),
            agent_type: "reviewer".to_string(),
            task: "look into flaky test".to_string(),
            label: "Investigate flaky test".to_string(),
        }),
        &empty_textarea(),
    );
    assert!(state.subagent_conversations[&run_id].messages.is_empty());
}

#[test]
fn subagent_tool_pending_auto_switches_view_and_arms_auto_return() {
    let mut state = AppState::new();
    state.mode = TuiMode::Streaming;
    let run_id = tracked_subagent(&mut state);
    let id = Uuid::new_v4();
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolPending {
            id,
            name: "write".to_string(),
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
            run_id: Some(run_id),
        }),
        &empty_textarea(),
    );
    assert!(
        matches!(state.active_view, ConversationView::Subagent { run_id: r, .. } if r == run_id),
        "expected auto-switch to the subagent's view, got {:?}",
        state.active_view
    );
    assert!(state.auto_switched_to_subagent);

    // Resolving the pause (denial) auto-returns since the switch was
    // automatic, not a manual navigation.
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolDenied { id }),
        &empty_textarea(),
    );
    assert_eq!(state.active_view, ConversationView::Root);
    assert!(!state.auto_switched_to_subagent);
}

#[test]
fn subagent_tool_pending_for_a_different_running_subagent_auto_switches_away_from_a_finished_subagents_view_and_restores_it_on_resolve(
) {
    // Regression: with two subagents spawned, one already finished (and
    // the user manually drilled into its transcript) and a second still
    // running, an approval belonging to the *second* (background) subagent
    // must not be stranded behind the finished subagent's transcript — it
    // has to switch to the subagent that actually needs the approval, then
    // return to exactly the view the user was on before.
    let mut state = AppState::new();
    state.mode = TuiMode::Streaming;

    // First subagent finishes; user manually drills into its (now static)
    // transcript.
    let finished_run_id = tracked_subagent(&mut state);
    state.running_subagents.clear();
    state.switch_to_subagent_view(finished_run_id);
    assert!(!state.auto_switched_to_subagent);

    // Second subagent starts running in the background and hits a bash
    // approval.
    let running_run_id = Uuid::new_v4();
    state.start_subagent_run(crate::tui_reducer_state::SubagentRunInfo {
        run_id: running_run_id,
        bubble_id: Uuid::new_v4(),
        agent_type: "reviewer".to_string(),
        task: "run the test suite".to_string(),
        label: "Run tests".to_string(),
    });
    let id = Uuid::new_v4();
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolPending {
            id,
            name: "bash".to_string(),
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
            run_id: Some(running_run_id),
        }),
        &empty_textarea(),
    );

    assert!(
        matches!(state.active_view, ConversationView::Subagent { run_id: r, .. } if r == running_run_id),
        "expected auto-switch to the subagent awaiting approval, got {:?}",
        state.active_view
    );
    assert!(state.auto_switched_to_subagent);

    // Resolving the approval must return to the finished subagent's view —
    // not Root, since that's what the user was actually looking at.
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolDenied { id }),
        &empty_textarea(),
    );
    assert!(
        matches!(state.active_view, ConversationView::Subagent { run_id: r, .. } if r == finished_run_id),
        "expected to return to the previously-viewed finished subagent, got {:?}",
        state.active_view
    );
    assert!(!state.auto_switched_to_subagent);
}

#[test]
fn domain_clarify_pending_sets_tui_mode() {
    let mut state = AppState::new();
    state.clarify_banner_state.clarify_scroll_offset = 5; // stale value from a prior clarify banner
    let effects = update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ClarifyPending {
            id: Uuid::new_v4(),
            question: "Test?".to_string(),
            context: None,
            choices: None,
            run_id: None,
        }),
        &empty_textarea(),
    );
    assert!(effects.contains(&Effect::SaveInputStateAndClear));
    assert!(matches!(
        state.mode,
        TuiMode::ClarifyPending { ref question, choices: None, .. } if question == "Test?"
    ));
    // Scrolled to the bottom (clamped to max_scroll on render) so the
    // question and choices are visible immediately, not the stale offset.
    assert_eq!(state.clarify_banner_state.clarify_scroll_offset, usize::MAX);
}

#[test]
fn domain_pending_clarify_pending_resets_scroll_to_bottom() {
    let mut state = AppState::new();
    state.mode = TuiMode::Pending;
    state.clarify_banner_state.clarify_scroll_offset = 5; // stale value from a prior clarify banner
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ClarifyPending {
            id: Uuid::new_v4(),
            question: "Test?".to_string(),
            context: None,
            choices: None,
            run_id: None,
        }),
        &empty_textarea(),
    );
    assert!(matches!(state.mode, TuiMode::ClarifyPending { .. }));
    assert_eq!(state.clarify_banner_state.clarify_scroll_offset, usize::MAX);
}

#[test]
fn domain_streaming_clarify_pending_resets_scroll_to_bottom() {
    let mut state = AppState::new();
    state.mode = TuiMode::Streaming;
    state.clarify_banner_state.clarify_scroll_offset = 5; // stale value from a prior clarify banner
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ClarifyPending {
            id: Uuid::new_v4(),
            question: "Test?".to_string(),
            context: None,
            choices: None,
            run_id: None,
        }),
        &empty_textarea(),
    );
    assert!(matches!(state.mode, TuiMode::ClarifyPending { .. }));
    assert_eq!(state.clarify_banner_state.clarify_scroll_offset, usize::MAX);
}

#[test]
fn subagent_engine_configuration_is_retained_for_its_conversation_view() {
    let mut state = AppState::new();
    let run_id = Uuid::new_v4();

    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::SubagentEngineConfigured {
            run_id,
            engine: crate::llm::engine::EngineDisplayInfo {
                engine_name: "ollama/local2".to_string(),
                provider_connection: "ollama".to_string(),
                model: "subagent-model".to_string(),
                reasoning_effort: None,
                compaction_engine: None,
                inherits_parent_runtime: false,
            },
        }),
        &empty_textarea(),
    );

    assert_eq!(
        state.subagent_engine_displays.get(&run_id),
        Some(&crate::llm::engine::EngineDisplayInfo {
            engine_name: "ollama/local2".to_string(),
            provider_connection: "ollama".to_string(),
            model: "subagent-model".to_string(),
            reasoning_effort: None,
            compaction_engine: None,
            inherits_parent_runtime: false,
        })
    );
}

#[test]
fn header_bubble_stays_expanded_after_the_subagent_finishes() {
    // Regression: a bubble the user manually expanded while the subagent
    // was still running must stay expanded once it finishes — the user
    // may still want to inspect the header (task/result) after the fact.
    let bubble_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let mut state = AppState::new();

    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::SubagentStarted {
            run_id,
            call_id: bubble_id,
            agent_type: "reviewer".to_string(),
            task: "look into flaky test".to_string(),
            label: "Investigate flaky test".to_string(),
        }),
        &empty_textarea(),
    );
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolStart {
            id: bubble_id,
            name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
            run_id: None,
        }),
        &empty_textarea(),
    );
    assert!(
        matches!(&state.chat_entries[0], ChatEntry::ToolBubble { collapsed, .. } if *collapsed),
        "spawn_subagent bubbles start collapsed by default"
    );

    // User manually expands the header bubble while the subagent runs.
    state.switch_to_subagent_view(run_id);
    state.layout.header_bubble = ratatui::layout::Rect::new(0, 0, 40, 4);
    update(
        &mut state,
        TuiEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 5,
            row: 1,
            modifiers: KeyModifiers::NONE,
        }),
        &empty_textarea(),
    );
    update(
        &mut state,
        TuiEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
            column: 5,
            row: 1,
            modifiers: KeyModifiers::NONE,
        }),
        &empty_textarea(),
    );
    assert!(
        matches!(&state.chat_entries[0], ChatEntry::ToolBubble { collapsed, .. } if !*collapsed),
        "click must have expanded the header bubble"
    );

    // The subagent finishes: its final `ToolDone` (the spawn_subagent
    // call itself completing) and `SubagentDone` (bookkeeping) arrive.
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolDone {
            id: bubble_id,
            name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
            result_section: ResultSection::plain(String::new()),
            summary: vec![],
            duration: std::time::Duration::from_secs(1),
            content_sections: vec![],
            run_id: None,
        }),
        &empty_textarea(),
    );
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::SubagentDone {
            run_id,
            status: "done".to_string(),
            result: None,
        }),
        &empty_textarea(),
    );

    assert!(
        matches!(&state.chat_entries[0], ChatEntry::ToolBubble { collapsed, .. } if !*collapsed),
        "the header bubble must still be expanded after the subagent finishes"
    );
}

#[test]
fn domain_stream_done_transitions_mode_to_idle() {
    let mut state = AppState::new();
    state.mode = TuiMode::Streaming;
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::StreamDone),
        &empty_textarea(),
    );
    assert_eq!(state.mode, TuiMode::Idle);
}

#[test]
fn domain_stream_done_preserves_manual_scroll_position() {
    // Regression test: a response completing must not override a
    // manual scroll-up (e.g. to view an earlier subagent bubble) by
    // force-snapping the chat back to the bottom.
    let mut state = AppState::new();
    state.mode = TuiMode::Streaming;
    state.conversation.scroll_locked = false;
    state.conversation.scroll_offset = 12;
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::StreamDone),
        &empty_textarea(),
    );
    assert!(!state.conversation.scroll_locked);
    assert_eq!(state.conversation.scroll_offset, 12);
}

#[test]
fn domain_stream_cancelled_preserves_manual_scroll_position() {
    let mut state = AppState::new();
    state.mode = TuiMode::Streaming;
    state.conversation.scroll_locked = false;
    state.conversation.scroll_offset = 12;
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::StreamCancelled),
        &empty_textarea(),
    );
    assert!(!state.conversation.scroll_locked);
    assert_eq!(state.conversation.scroll_offset, 12);
}

#[test]
fn domain_stream_thinking_chunk_appends_to_partial_thinking_text_while_streaming() {
    let mut state = AppState::new();
    state.mode = TuiMode::Streaming;
    state.conversation.scroll_locked = true;
    state.conversation.scroll_offset = 7;
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::StreamThinkingChunk("pondering...".to_string())),
        &empty_textarea(),
    );
    assert_eq!(state.turn_buffer.partial_thinking_text, "pondering...");
    assert!(
        state.turn_buffer.partial_assistant_text.is_empty(),
        "thinking must not contaminate the assistant text accumulator"
    );
    assert_eq!(
        state.conversation.scroll_offset, 0,
        "scroll-locked must snap to bottom"
    );
}

#[test]
fn domain_streaming_interleaved_thinking_and_response_flush_into_separate_bubbles_in_order() {
    // A turn that alternates thinking → response → thinking with no tool
    // call in between must still split into one bubble per segment, in
    // chronological order, rather than merging every thinking fragment
    // into one bubble and every response fragment into another.
    let mut state = AppState::new();
    state.mode = TuiMode::Streaming;
    for event in [
        DomainEvent::StreamThinkingChunk("first thought".to_string()),
        DomainEvent::StreamChunk("first reply".to_string()),
        DomainEvent::StreamThinkingChunk("second thought".to_string()),
        DomainEvent::StreamChunk("second reply".to_string()),
        DomainEvent::StreamDone,
    ] {
        update(&mut state, TuiEvent::Domain(event), &empty_textarea());
    }

    assert_eq!(state.chat_entries.len(), 4, "{:?}", state.chat_entries);
    assert!(matches!(
        &state.chat_entries[0],
        ChatEntry::ThinkingMessage { text, .. } if text == "first thought"
    ));
    assert!(matches!(
        &state.chat_entries[1],
        ChatEntry::AssistantMessage { text, .. } if text == "first reply"
    ));
    assert!(matches!(
        &state.chat_entries[2],
        ChatEntry::ThinkingMessage { text, .. } if text == "second thought"
    ));
    assert!(matches!(
        &state.chat_entries[3],
        ChatEntry::AssistantMessage { text, .. } if text == "second reply"
    ));
}

#[test]
fn subagent_token_interleaved_thinking_and_response_flush_into_separate_bubbles_in_order() {
    // Mirrors `domain_streaming_interleaved_thinking_and_response_flush_into_separate_bubbles_in_order`
    // for a subagent conversation: `SubagentToken`'s `is_thinking` flag must
    // route each fragment to the subagent's own `TurnBuffer` the same way the
    // root's `StreamThinkingChunk`/`StreamChunk` do, rather than merging
    // thinking and response text into a single blob (the bug this test
    // guards against).
    let mut state = AppState::new();
    let run_id = Uuid::new_v4();
    state.start_subagent_run(SubagentRunInfo {
        run_id,
        bubble_id: Uuid::new_v4(),
        agent_type: "reviewer".to_string(),
        task: "look into flaky test".to_string(),
        label: "Investigate flaky test".to_string(),
    });
    for (text, is_thinking) in [
        ("first thought", true),
        ("first reply", false),
        ("second thought", true),
        ("second reply", false),
    ] {
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::SubagentToken {
                run_id,
                text: text.to_string(),
                is_thinking,
            }),
            &empty_textarea(),
        );
    }
    state.flush_subagent_partial_text(run_id);

    let conversation = state
        .subagent_conversations
        .get(&run_id)
        .expect("conversation");
    assert_eq!(
        conversation.messages.len(),
        4,
        "{:?}",
        conversation.messages
    );
    assert!(matches!(
        &conversation.messages[0],
        ChatEntry::ThinkingMessage { text, .. } if text == "first thought"
    ));
    assert!(matches!(
        &conversation.messages[1],
        ChatEntry::AssistantMessage { text, .. } if text == "first reply"
    ));
    assert!(matches!(
        &conversation.messages[2],
        ChatEntry::ThinkingMessage { text, .. } if text == "second thought"
    ));
    assert!(matches!(
        &conversation.messages[3],
        ChatEntry::AssistantMessage { text, .. } if text == "second reply"
    ));
}

#[test]
fn subagent_context_usage_is_kept_per_run_and_selected_only_for_drilled_view() {
    let mut state = AppState::new();
    let first_run_id = Uuid::new_v4();
    let second_run_id = Uuid::new_v4();
    for (run_id, label) in [(first_run_id, "First"), (second_run_id, "Second")] {
        state.start_subagent_run(SubagentRunInfo {
            run_id,
            bubble_id: Uuid::new_v4(),
            agent_type: "reviewer".to_string(),
            task: format!("Review {label}"),
            label: label.to_string(),
        });
    }
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::SubagentContextUsage {
            run_id: first_run_id,
            estimated_tokens: 2_000,
            context_length: 16_000,
            source: ContextUsageSource::SerializedRequest,
        }),
        &empty_textarea(),
    );
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::SubagentContextUsage {
            run_id: second_run_id,
            estimated_tokens: 6_000,
            context_length: 32_000,
            source: ContextUsageSource::ModelView,
        }),
        &empty_textarea(),
    );

    assert_eq!(
        state.context_usage, None,
        "a background subagent's report must not overwrite root usage"
    );
    assert_eq!(
        state.active_context_usage(),
        None,
        "root view selects root usage"
    );

    state.switch_to_subagent_view(first_run_id);
    assert_eq!(
        state.active_context_usage(),
        Some(ContextUsage {
            estimated_tokens: 2_000,
            context_length: 16_000,
            source: ContextUsageSource::SerializedRequest,
        })
    );
    state.switch_to_subagent_view(second_run_id);
    assert_eq!(
        state.active_context_usage(),
        Some(ContextUsage {
            estimated_tokens: 6_000,
            context_length: 32_000,
            source: ContextUsageSource::ModelView,
        })
    );

    state.switch_to_root_view();
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ContextUsage {
            estimated_tokens: 8_000,
            context_length: 64_000,
            source: ContextUsageSource::ModelView,
        }),
        &empty_textarea(),
    );
    assert_eq!(
        state.active_context_usage(),
        Some(ContextUsage {
            estimated_tokens: 8_000,
            context_length: 64_000,
            source: ContextUsageSource::ModelView,
        }),
        "root view returns to root usage"
    );

    state.switch_to_subagent_view(first_run_id);
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::SubagentDone {
            run_id: first_run_id,
            status: "success".to_string(),
            result: Some("done".to_string()),
        }),
        &empty_textarea(),
    );
    assert_eq!(
        state.active_context_usage(),
        Some(ContextUsage {
            estimated_tokens: 2_000,
            context_length: 16_000,
            source: ContextUsageSource::SerializedRequest,
        }),
        "finished subagent readout remains available while its transcript is drilled in"
    );
}

#[test]
fn compaction_activity_is_scoped_and_preserves_agent_mode() {
    let mut state = AppState::new();
    let root_request_id = Uuid::new_v4();
    let subagent_run_id = Uuid::new_v4();
    let subagent_request_id = Uuid::new_v4();
    for (run_id, request_id) in [
        (None, root_request_id),
        (Some(subagent_run_id), subagent_request_id),
    ] {
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::CompactionActivity {
                run_id,
                request_id,
                active: true,
            }),
            &empty_textarea(),
        );
    }
    assert_eq!(state.mode, TuiMode::Idle);
    assert_eq!(state.compaction_requests.len(), 2);
    for request_id in [Uuid::new_v4(), root_request_id] {
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::CompactionActivity {
                run_id: None,
                request_id,
                active: false,
            }),
            &empty_textarea(),
        );
        if request_id != root_request_id {
            assert_eq!(state.compaction_requests.get(&None), Some(&root_request_id));
        }
    }
    assert!(!state.compaction_requests.contains_key(&None));
    assert_eq!(
        state.compaction_requests.get(&Some(subagent_run_id)),
        Some(&subagent_request_id)
    );
    state.clear_conversation();
    assert!(state.compaction_requests.is_empty());
}

#[test]
fn compaction_replaces_the_stale_root_footer_value() {
    let mut state = AppState::new();
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ContextUsage {
            estimated_tokens: 76_915,
            context_length: 98_304,
            source: ContextUsageSource::SerializedRequest,
        }),
        &empty_textarea(),
    );
    assert!(state.context_usage.is_some());

    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ContextUsage {
            estimated_tokens: 9_854,
            context_length: 98_304,
            source: ContextUsageSource::ModelView,
        }),
        &empty_textarea(),
    );

    assert_eq!(
        state.active_context_usage(),
        Some(ContextUsage {
            estimated_tokens: 9_854,
            context_length: 98_304,
            source: ContextUsageSource::ModelView,
        }),
        "post-compaction model-view estimate replaces the stale request estimate"
    );
}

#[test]
fn compaction_usage_update_does_not_interrupt_the_follow_up_response_state() {
    let mut state = AppState::new();
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::StreamDone),
        &empty_textarea(),
    );
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ContextUsage {
            estimated_tokens: 9_854,
            context_length: 98_304,
            source: ContextUsageSource::ModelView,
        }),
        &empty_textarea(),
    );
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::StreamStarted),
        &empty_textarea(),
    );

    assert_eq!(state.mode, TuiMode::Streaming);
    assert_eq!(
        state.active_context_usage().map(|usage| usage.source),
        Some(ContextUsageSource::ModelView)
    );
}

#[test]
fn domain_stream_started_clears_partial_thinking_text() {
    let mut state = AppState::new();
    state.mode = TuiMode::Pending;
    state.turn_buffer.partial_thinking_text = "stale thinking".to_string();
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::StreamStarted),
        &empty_textarea(),
    );
    assert!(state.turn_buffer.partial_thinking_text.is_empty());
}

#[test]
fn confirm_expired_event_clears_double_press_abort_mode() {
    let mut state = AppState::new();
    state.mode = TuiMode::DoublePressAbort {
        pressed_at: Instant::now(),
        return_to: Box::new(TuiMode::Streaming),
        armed_by: ConfirmKey::CtrlC,
    };
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ConfirmExpired),
        &empty_textarea(),
    );
    assert_eq!(state.mode, TuiMode::Streaming);
}

#[test]
fn stream_chunk_during_double_press_abort_preserves_confirm_window() {
    // A StreamChunk must not destroy the DoublePressAbort mode — the user
    // must still be able to land the second Esc while the model streams.
    let mut state = AppState::new();
    let pressed_at = Instant::now();
    state.mode = TuiMode::DoublePressAbort {
        pressed_at,
        return_to: Box::new(TuiMode::Streaming),
        armed_by: ConfirmKey::CtrlC,
    };
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::StreamChunk("hello".to_string())),
        &empty_textarea(),
    );
    assert!(
        matches!(state.mode, TuiMode::DoublePressAbort { .. }),
        "DoublePressAbort must survive a StreamChunk"
    );
    assert_eq!(
        state.turn_buffer.partial_assistant_text, "hello",
        "chunk must still be appended"
    );
}
