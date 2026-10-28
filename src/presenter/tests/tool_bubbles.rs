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
fn pending_and_running_transitions_share_bubble_semantics_across_root_modes() {
    for mode in [TuiMode::Idle, TuiMode::Pending, TuiMode::Streaming] {
        let mut state = AppState::new();
        state.mode = mode.clone();
        state.turn_buffer.partial_assistant_text = "before tool".into();
        let tool_id = Uuid::new_v4();
        let arguments = serde_json::json!({"path": "file.txt"});
        let title_arguments = vec![("path".into(), serde_json::json!("file.txt"))];

        assert_eq!(
            update(
                &mut state,
                TuiEvent::Domain(DomainEvent::ToolPending {
                    id: tool_id,
                    name: "write".into(),
                    arguments: arguments.clone(),
                    content_sections: vec![],
                    title_arguments: title_arguments.clone(),
                    run_id: None,
                }),
                &empty_textarea(),
            ),
            [Effect::ParkSlashOverlay]
        );
        assert!(state.turn_buffer.partial_assistant_text.is_empty());
        assert!(matches!(
            &state.chat_entries[0],
            ChatEntry::AssistantMessage { text, .. } if text == "before tool"
        ));
        assert_eq!(state.mode, mode);
        assert_eq!(
            state.focus,
            Focus::ToolApproval {
                tool_id,
                tool_name: "write".into(),
            }
        );
        assert!(update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolStart {
                id: tool_id,
                name: "write".into(),
                arguments: arguments.clone(),
                content_sections: vec![],
                title_arguments: title_arguments.clone(),
                run_id: None,
            }),
            &empty_textarea(),
        )
        .is_empty());
        assert_eq!(state.chat_entries.len(), 2);
        assert!(matches!(
            &state.chat_entries[1],
            ChatEntry::ToolBubble { id, state, .. }
                if *id == tool_id && matches!(state.as_ref(),
                    ToolCallState::Running {
                        arguments: actual_arguments,
                        title_arguments: actual_title_arguments,
                        progress_arguments,
                        ..
                    } if *actual_arguments == arguments
                        && *actual_title_arguments == title_arguments
                        && progress_arguments.is_empty())
        ));
        assert_eq!(state.focus, Focus::MainInput);
        assert_eq!(state.mode, mode);
    }
}

#[test]
fn domain_tool_start_flushes_pending_thinking_before_new_tool_bubble() {
    // An auto-run tool call (no ToolPending approval step) must flush any
    // thinking accumulated so far into its own bubble *before* the tool
    // bubble is inserted, so the chat log reads in chronological order:
    // "thought, then called the tool" rather than "called the tool, then
    // thought" (the latter happened when thinking was only flushed on
    // StreamDone, at the very end of the whole tool-using turn).
    let mut state = AppState::new();
    state.mode = TuiMode::Streaming;
    state.turn_buffer.partial_thinking_text = "deciding to read the file".to_string();
    state.turn_buffer.thinking_started_at = Some(std::time::Instant::now());
    let tool_id = Uuid::new_v4();
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolStart {
            id: tool_id,
            name: "read".to_string(),
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
            run_id: None,
        }),
        &empty_textarea(),
    );

    assert!(
        state.turn_buffer.partial_thinking_text.is_empty(),
        "thinking must be flushed, not left dangling"
    );
    assert_eq!(
        state.chat_entries.len(),
        2,
        "expected [ThinkingMessage, ToolBubble]"
    );
    assert!(matches!(
        &state.chat_entries[0],
        ChatEntry::ThinkingMessage { text, .. } if text == "deciding to read the file"
    ));
    assert!(matches!(
        &state.chat_entries[1],
        ChatEntry::ToolBubble { id, .. } if *id == tool_id
    ));
}

#[test]
fn domain_tool_progress_patches_progress_arguments_on_running_bubble() {
    // `ToolProgress` is an interim status update (e.g. `spawn_subagent`
    // reporting status=spawned/queued) — it must patch `progress_arguments`
    // on the existing `Running` bubble (not `title_arguments`, which stays
    // the call's real, static arguments) without transitioning it to a
    // terminal state (spinner keeps going, content sections untouched).
    let mut state = AppState::new();
    state.mode = TuiMode::Streaming;
    let tool_id = Uuid::new_v4();
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolStart {
            id: tool_id,
            name: "spawn_subagent".to_string(),
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![(
                "label".to_string(),
                serde_json::json!("Refactor auth token validation"),
            )],
            run_id: None,
        }),
        &empty_textarea(),
    );

    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolProgress {
            id: tool_id,
            title_arguments: vec![
                ("status".to_string(), serde_json::json!("running")),
                ("run_id".to_string(), serde_json::json!("abc-123")),
            ],
        }),
        &empty_textarea(),
    );

    let entry = state
        .chat_entries
        .iter()
        .find(|entry| matches!(entry, ChatEntry::ToolBubble { id, .. } if *id == tool_id))
        .expect("tool bubble should still exist");
    match entry {
        ChatEntry::ToolBubble { state: bubble, .. } => match bubble.as_ref() {
            ToolCallState::Running {
                title_arguments,
                progress_arguments,
                ..
            } => {
                assert_eq!(
                    title_arguments,
                    &vec![(
                        "label".to_string(),
                        serde_json::json!("Refactor auth token validation")
                    )],
                    "the call's real title_arguments must be untouched by ToolProgress"
                );
                assert_eq!(
                    progress_arguments,
                    &vec![
                        ("status".to_string(), serde_json::json!("running")),
                        ("run_id".to_string(), serde_json::json!("abc-123")),
                    ]
                );
            }
            other => panic!("expected the bubble to remain Running, got: {other:?}"),
        },
        _ => unreachable!(),
    }
}

#[test]
fn domain_tool_done_does_not_inherit_stale_progress_arguments() {
    // Regression test: once ToolDone arrives, any interim
    // `progress_arguments` (e.g. status=spawned/run_id=...) must not
    // linger in the terminal bubble — only the call's real
    // `title_arguments` (e.g. `label`) survive, and the final status
    // comes solely from `summary`.
    let mut state = AppState::new();
    state.mode = TuiMode::Streaming;
    let tool_id = Uuid::new_v4();
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolStart {
            id: tool_id,
            name: "spawn_subagent".to_string(),
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![(
                "label".to_string(),
                serde_json::json!("Refactor auth token validation"),
            )],
            run_id: None,
        }),
        &empty_textarea(),
    );
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolProgress {
            id: tool_id,
            title_arguments: vec![
                ("status".to_string(), serde_json::json!("running")),
                ("run_id".to_string(), serde_json::json!("abc-123")),
            ],
        }),
        &empty_textarea(),
    );
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolDone {
            id: tool_id,
            name: "spawn_subagent".to_string(),
            result_section: crate::tui_reducer_state::ResultSection::plain(String::new()),
            content_sections: vec![],
            summary: vec![("status".to_string(), "unstructured".to_string())],
            duration: std::time::Duration::from_millis(1500),
            run_id: None,
        }),
        &empty_textarea(),
    );

    let entry = state
        .chat_entries
        .iter()
        .find(|entry| matches!(entry, ChatEntry::ToolBubble { id, .. } if *id == tool_id))
        .expect("tool bubble should still exist");
    match entry {
        ChatEntry::ToolBubble { state: bubble, .. } => match bubble.as_ref() {
            ToolCallState::Done {
                title_arguments,
                summary,
                duration,
                ..
            } => {
                assert_eq!(
                    title_arguments,
                    &vec![(
                        "label".to_string(),
                        serde_json::json!("Refactor auth token validation")
                    )],
                    "the stale spawned/run_id progress values must not leak into Done"
                );
                assert_eq!(
                    summary,
                    &vec![("status".to_string(), "unstructured".to_string())]
                );
                assert_eq!(*duration, std::time::Duration::from_millis(1500));
            }
            other => panic!("expected the bubble to be Done, got: {other:?}"),
        },
        _ => unreachable!(),
    }
}

#[test]
fn domain_tool_progress_is_a_no_op_when_bubble_is_not_running() {
    // Once a bubble reached a terminal state, a stray/late ToolProgress
    // must not resurrect or otherwise mutate it.
    let mut state = AppState::new();
    state.mode = TuiMode::Streaming;
    let tool_id = Uuid::new_v4();
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolStart {
            id: tool_id,
            name: "spawn_subagent".to_string(),
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
            run_id: None,
        }),
        &empty_textarea(),
    );
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolDone {
            id: tool_id,
            name: "spawn_subagent".to_string(),
            result_section: crate::tui_reducer_state::ResultSection::plain(String::new()),
            content_sections: vec![],
            summary: vec![("status".to_string(), "success".to_string())],
            duration: std::time::Duration::ZERO,
            run_id: None,
        }),
        &empty_textarea(),
    );

    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolProgress {
            id: tool_id,
            title_arguments: vec![("status".to_string(), serde_json::json!("running"))],
        }),
        &empty_textarea(),
    );

    let entry = state
        .chat_entries
        .iter()
        .find(|entry| matches!(entry, ChatEntry::ToolBubble { id, .. } if *id == tool_id))
        .expect("tool bubble should still exist");
    match entry {
        ChatEntry::ToolBubble { state: bubble, .. } => {
            assert!(
                matches!(bubble.as_ref(), ToolCallState::Done { .. }),
                "a late ToolProgress must not move the bubble out of Done, got: {bubble:?}"
            );
        }
        _ => unreachable!(),
    }
}

#[test]
fn domain_tool_pending_while_aborting_returns_deny_effect() {
    let mut state = AppState::new();
    state.mode = TuiMode::Aborting;
    let tool_id = Uuid::new_v4();
    let effects = update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolPending {
            id: tool_id,
            name: "test_tool".to_string(),
            arguments: serde_json::json!({}),
            content_sections: vec![],

            title_arguments: vec![],
            run_id: None,
        }),
        &empty_textarea(),
    );
    assert!(effects.contains(&Effect::DenyTool {
        id: tool_id,
        reason: String::new(),
    }));
    // Focus must not change to ToolApproval.
    assert_ne!(
        state.focus,
        Focus::ToolApproval {
            tool_id,
            tool_name: "test_tool".to_string()
        }
    );
}

#[test]
fn tool_pending_parks_the_input_state_via_the_save_effect() {
    // Regression: a half-typed slash command left the suggestion overlay
    // open when a tool approval arrived, and the approval footer draws on
    // the same rows above the input. Arming the approval must emit the
    // save effect that parks draft + overlay together and hides the
    // overlay (the snapshot/hide itself happens when the Driver executes
    // the effect — covered by the `tui_driver` tests).
    let mut state = AppState::new();
    state.refresh_slash_overlay("/mo");
    assert!(
        matches!(state.slash_overlay, SlashOverlay::Suggestions { .. }),
        "expected the suggestion overlay to be open, got {:?}",
        state.slash_overlay
    );

    let effects = update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolPending {
            id: Uuid::new_v4(),
            name: "write".to_string(),
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
            run_id: None,
        }),
        &textarea_with("/mo"),
    );
    assert!(matches!(state.focus, Focus::ToolApproval { .. }));
    assert!(
        effects.contains(&Effect::ParkSlashOverlay),
        "expected the input-state save effect, got {effects:?}"
    );
}

#[test]
fn denied_tool_bubble_retains_arguments() {
    let mut state = AppState::new();
    state.mode = TuiMode::Idle;
    let tool_id = Uuid::new_v4();
    let args = serde_json::json!({"path": "/tmp/foo", "content": "bar"});
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolPending {
            id: tool_id,
            name: "write_file".to_string(),
            arguments: args.clone(),
            content_sections: vec![],

            title_arguments: vec![],
            run_id: None,
        }),
        &empty_textarea(),
    );
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolDenied { id: tool_id }),
        &empty_textarea(),
    );
    let denied_bubble = state.chat_entries.iter().find_map(|entry| {
        let ChatEntry::ToolBubble { state, .. } = entry else {
            return None;
        };
        if let ToolCallState::Denied { arguments, .. } = state.as_ref() {
            return Some(arguments.clone());
        }
        None
    });
    assert_eq!(
        denied_bubble.expect("denied bubble should exist"),
        args,
        "Denied bubble must retain the arguments from the Pending state"
    );
}

#[test]
fn approval_arriving_while_typing_waits_until_typing_settles() {
    let mut state = AppState::new();
    state.note_keystroke(std::time::Instant::now());
    let tool_id = Uuid::new_v4();
    let effects = update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolPending {
            id: tool_id,
            name: "write".into(),
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
            run_id: None,
        }),
        &empty_textarea(),
    );
    assert!(effects.is_empty(), "{effects:?}");
    assert_eq!(state.focus, Focus::MainInput);
    assert_eq!(state.deferred_prompt_count(), 1);

    let effects = update(
        &mut state,
        TuiEvent::Domain(DomainEvent::TypingSettled),
        &empty_textarea(),
    );
    assert_eq!(effects, [Effect::ParkSlashOverlay]);
    assert!(matches!(
        state.focus,
        Focus::ToolApproval { tool_id: shown, .. } if shown == tool_id
    ));
    assert_eq!(state.deferred_prompt_count(), 0);
}

#[test]
fn clarify_arriving_while_typing_waits_until_typing_settles() {
    let mut state = AppState::new();
    state.note_keystroke(std::time::Instant::now());
    let clarify_id = Uuid::new_v4();
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ClarifyPending {
            id: clarify_id,
            question: "Which?".into(),
            context: None,
            choices: None,
            run_id: None,
        }),
        &empty_textarea(),
    );
    assert!(!matches!(state.mode, TuiMode::ClarifyPending { .. }));
    assert_eq!(state.deferred_prompt_count(), 1);

    let effects = update(
        &mut state,
        TuiEvent::Domain(DomainEvent::TypingSettled),
        &empty_textarea(),
    );
    assert_eq!(effects, [Effect::ParkSlashOverlay]);
    assert!(matches!(state.mode, TuiMode::ClarifyPending { .. }));
}

fn auto_expand_state(tool_names: &[&str], approval: bool) -> AppState {
    let mut state = AppState::new();
    state.mode = TuiMode::Streaming;
    state.tui_config.auto_expand_tool = tool_names.iter().map(ToString::to_string).collect();
    state.tui_config.auto_expand_approval = approval;
    state
}

fn tool_pending_event(id: Uuid, name: &str, run_id: Option<Uuid>) -> TuiEvent {
    TuiEvent::Domain(DomainEvent::ToolPending {
        id,
        name: name.into(),
        arguments: serde_json::json!({}),
        content_sections: vec![],
        title_arguments: vec![],
        run_id,
    })
}

fn tool_start_event(id: Uuid, name: &str, run_id: Option<Uuid>) -> TuiEvent {
    TuiEvent::Domain(DomainEvent::ToolStart {
        id,
        name: name.into(),
        arguments: serde_json::json!({}),
        content_sections: vec![],
        title_arguments: vec![],
        run_id,
    })
}

fn bubble_collapsed(state: &mut AppState, id: Uuid) -> bool {
    match state.find_tool_bubble_mut(id) {
        Some(ChatEntry::ToolBubble { collapsed, .. }) => *collapsed,
        _ => panic!("bubble {id} not found"),
    }
}

#[test]
fn listed_tools_arrive_expanded_in_every_creation_path_and_others_stay_collapsed() {
    let mut state = auto_expand_state(&["read", "write"], false);
    let (pending, running, done, other) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    update(
        &mut state,
        tool_pending_event(pending, "write", None),
        &empty_textarea(),
    );
    update(
        &mut state,
        tool_start_event(running, "read", None),
        &empty_textarea(),
    );
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolDone {
            id: done,
            name: "read".into(),
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
        tool_start_event(other, "bash", None),
        &empty_textarea(),
    );

    assert!(!bubble_collapsed(&mut state, pending));
    assert!(!bubble_collapsed(&mut state, running));
    assert!(!bubble_collapsed(&mut state, done));
    assert!(bubble_collapsed(&mut state, other));
    assert!(state.pending_bubble_expansion.is_none());
}

#[test]
fn arrival_expansion_keeps_a_bottom_pinned_view_pinned_and_matches_names_exactly() {
    let mut state = auto_expand_state(&["read"], false);
    state.conversation.scroll_locked = true;
    let (exact, longer) = (Uuid::new_v4(), Uuid::new_v4());
    update(
        &mut state,
        tool_start_event(exact, "read", None),
        &empty_textarea(),
    );
    update(
        &mut state,
        tool_start_event(longer, "read_more", None),
        &empty_textarea(),
    );
    assert!(!bubble_collapsed(&mut state, exact));
    assert!(bubble_collapsed(&mut state, longer));
    assert!(state.conversation.scroll_locked);
}

#[test]
fn defaults_keep_every_tool_bubble_collapsed() {
    let mut state = auto_expand_state(&[], false);
    let id = Uuid::new_v4();
    update(
        &mut state,
        tool_pending_event(id, "write", None),
        &empty_textarea(),
    );
    assert!(bubble_collapsed(&mut state, id));
}

#[test]
fn subagent_bubbles_follow_arrival_expansion() {
    let mut state = auto_expand_state(&["edit"], false);
    let run_id = tracked_subagent(&mut state);
    let id = Uuid::new_v4();
    update(
        &mut state,
        tool_start_event(id, "edit", Some(run_id)),
        &empty_textarea(),
    );
    assert!(!bubble_collapsed(&mut state, id));
}

#[test]
fn approval_expands_the_pending_bubble_and_collapses_it_when_approved() {
    let mut state = auto_expand_state(&[], true);
    let id = Uuid::new_v4();
    update(
        &mut state,
        tool_pending_event(id, "bash", None),
        &empty_textarea(),
    );
    assert!(!bubble_collapsed(&mut state, id));
    assert!(state.pending_bubble_expansion.is_none());

    update(
        &mut state,
        tool_start_event(id, "bash", None),
        &empty_textarea(),
    );
    assert!(bubble_collapsed(&mut state, id));
    assert!(state.approval_auto_expanded.is_empty());
}

#[test]
fn approval_expansion_collapses_after_denial() {
    let mut state = auto_expand_state(&[], true);
    let id = Uuid::new_v4();
    update(
        &mut state,
        tool_pending_event(id, "bash", None),
        &empty_textarea(),
    );
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolDenied { id }),
        &empty_textarea(),
    );
    assert!(bubble_collapsed(&mut state, id));
}

#[test]
fn approval_expansion_is_off_by_default() {
    let mut state = auto_expand_state(&[], false);
    let id = Uuid::new_v4();
    update(
        &mut state,
        tool_pending_event(id, "bash", None),
        &empty_textarea(),
    );
    assert!(bubble_collapsed(&mut state, id));
}

#[test]
fn queued_approval_expands_only_once_it_is_promoted() {
    let mut state = auto_expand_state(&[], true);
    let (first, second) = (Uuid::new_v4(), Uuid::new_v4());
    update(
        &mut state,
        tool_pending_event(first, "bash", None),
        &empty_textarea(),
    );
    update(
        &mut state,
        tool_pending_event(second, "bash", None),
        &empty_textarea(),
    );
    assert!(!bubble_collapsed(&mut state, first));
    assert!(bubble_collapsed(&mut state, second));

    update(
        &mut state,
        tool_start_event(first, "bash", None),
        &empty_textarea(),
    );
    assert!(bubble_collapsed(&mut state, first));
    assert!(!bubble_collapsed(&mut state, second));
}

#[test]
fn approval_arriving_while_typing_expands_at_promotion() {
    let mut state = auto_expand_state(&[], true);
    state.note_keystroke(std::time::Instant::now());
    let id = Uuid::new_v4();
    update(
        &mut state,
        tool_pending_event(id, "bash", None),
        &empty_textarea(),
    );
    assert!(bubble_collapsed(&mut state, id));
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::TypingSettled),
        &empty_textarea(),
    );
    assert!(!bubble_collapsed(&mut state, id));
}

#[test]
fn subagent_approval_expands_and_collapses() {
    let mut state = auto_expand_state(&[], true);
    let run_id = tracked_subagent(&mut state);
    let id = Uuid::new_v4();
    update(
        &mut state,
        tool_pending_event(id, "bash", Some(run_id)),
        &empty_textarea(),
    );
    assert!(!bubble_collapsed(&mut state, id));
    update(
        &mut state,
        tool_start_event(id, "bash", Some(run_id)),
        &empty_textarea(),
    );
    assert!(bubble_collapsed(&mut state, id));
}

#[test]
fn manual_toggle_during_approval_is_kept_after_resolution() {
    let mut state = auto_expand_state(&[], true);
    let id = Uuid::new_v4();
    update(
        &mut state,
        tool_pending_event(id, "bash", None),
        &empty_textarea(),
    );
    state.toggle_tool_bubble_collapsed(id);
    assert!(bubble_collapsed(&mut state, id));
    state.toggle_tool_bubble_collapsed(id);
    assert!(!bubble_collapsed(&mut state, id));

    update(
        &mut state,
        tool_start_event(id, "bash", None),
        &empty_textarea(),
    );
    assert!(!bubble_collapsed(&mut state, id));
}

#[test]
fn listed_tool_stays_expanded_after_approval() {
    let mut state = auto_expand_state(&["bash"], true);
    let id = Uuid::new_v4();
    update(
        &mut state,
        tool_pending_event(id, "bash", None),
        &empty_textarea(),
    );
    update(
        &mut state,
        tool_start_event(id, "bash", None),
        &empty_textarea(),
    );
    assert!(!bubble_collapsed(&mut state, id));
}
