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
async fn effect_batches_keep_input_and_command_order_and_stop_at_quit() {
    let (mut state, mut textarea, cmd_tx, mut cmd_rx) = setup();
    let outcome = execute_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        vec![
            Effect::SetInputText("  first prompt".to_string()),
            Effect::SubmitPrompt { steer: false },
            Effect::ClearTextarea,
            Effect::Paste("  second\n  prompt".to_string()),
            Effect::SubmitPrompt { steer: false },
            Effect::Quit,
            Effect::ClearTextarea,
            Effect::StopAgent,
        ],
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut PendingRunIds::default(),
    )
    .await;

    assert_eq!(outcome, ActionOutcome::Quit);
    assert_eq!(
        cmd_rx.try_recv(),
        Ok(AgentCommand::Prompt("first prompt".to_string()))
    );
    assert_eq!(
        cmd_rx.try_recv(),
        Ok(AgentCommand::Prompt("second\nprompt".to_string()))
    );
    assert_eq!(textarea.lines(), &["second", "prompt"]);
    assert!(cmd_rx.try_recv().is_err());
}

#[tokio::test]
async fn aborting_tool_error_waits_for_idle_before_quitting() {
    let (mut state, mut textarea, cmd_tx, _cmd_rx) = setup();
    let tool_id = Uuid::new_v4();
    state.mode = TuiMode::Aborting;
    state.aborting_exit = true;
    state.focus = Focus::ToolApproval {
        tool_id,
        tool_name: "bash".to_string(),
    };
    state.chat_entries.push(ChatEntry::ToolBubble {
        id: tool_id,
        name: "bash".to_string(),
        state: Box::new(ToolCallState::Pending {
            arguments: serde_json::json!({"command": "echo test"}),
            content_sections: vec![],
            title_arguments: vec![],
        }),
        collapsed: false,
    });

    let textarea_context = textarea_ctx_from(&textarea);
    let outcome = process_one_update(
        &mut state,
        &mut textarea,
        &cmd_tx,
        &textarea_context,
        &UiUpdate::ToolError {
            id: tool_id,
            error: "aborted by user".to_string(),
            duration: Duration::ZERO,
            content_sections: vec![],
            run_id: None,
        },
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut PendingRunIds::default(),
    )
    .await;

    assert_eq!(outcome, ActionOutcome::Continue);
    assert_eq!(
        state.mode,
        TuiMode::Aborting,
        "the terminal tool error must not bypass the aborting state"
    );

    let idle_outcome = process_one_update(
        &mut state,
        &mut textarea,
        &cmd_tx,
        &textarea_context,
        &UiUpdate::AgentIdle,
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut PendingRunIds::default(),
    )
    .await;
    assert_eq!(
        idle_outcome,
        ActionOutcome::Quit,
        "the following idle event should complete a Ctrl-C abort"
    );
}

#[tokio::test]
async fn aborting_tool_error_then_idle_keeps_tui_open_for_escape() {
    let (mut state, mut textarea, cmd_tx, _cmd_rx) = setup();
    let tool_id = Uuid::new_v4();
    state.mode = TuiMode::Aborting;
    state.focus = Focus::ToolApproval {
        tool_id,
        tool_name: "bash".to_string(),
    };
    state.chat_entries.push(ChatEntry::ToolBubble {
        id: tool_id,
        name: "bash".to_string(),
        state: Box::new(ToolCallState::Pending {
            arguments: serde_json::json!({"command": "echo test"}),
            content_sections: vec![],
            title_arguments: vec![],
        }),
        collapsed: false,
    });

    let textarea_context = textarea_ctx_from(&textarea);
    let tool_error_outcome = process_one_update(
        &mut state,
        &mut textarea,
        &cmd_tx,
        &textarea_context,
        &UiUpdate::ToolError {
            id: tool_id,
            error: "aborted by user".to_string(),
            duration: Duration::ZERO,
            content_sections: vec![],
            run_id: None,
        },
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut PendingRunIds::default(),
    )
    .await;
    assert_eq!(tool_error_outcome, ActionOutcome::Continue);

    let idle_outcome = process_one_update(
        &mut state,
        &mut textarea,
        &cmd_tx,
        &textarea_context,
        &UiUpdate::AgentIdle,
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut PendingRunIds::default(),
    )
    .await;
    assert_eq!(idle_outcome, ActionOutcome::Continue);
    assert_eq!(state.mode, TuiMode::Idle);
    assert_eq!(state.focus, Focus::MainInput);
    assert!(!state.aborting_exit);
}

#[tokio::test]
async fn tool_pending_ctrl_c_denies_tool_immediately() {
    let (mut state, mut textarea, cmd_tx, mut cmd_rx) = setup();
    let tool_id = uuid::Uuid::new_v4();
    // Simulate ToolPending arriving via domain event.
    let effects = presenter::update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolPending {
            id: tool_id,
            name: "read".to_string(),
            arguments: serde_json::json!({}),
            content_sections: vec![],

            title_arguments: vec![],
            run_id: None,
        }),
        &textarea_ctx_from(&textarea),
    );
    execute_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        effects,
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut PendingRunIds::default(),
    )
    .await;
    assert!(matches!(state.focus, Focus::ToolApproval { .. }));

    let ctx = textarea_ctx_from(&textarea);
    let effects = presenter::update(&mut state, TuiEvent::Input(InputEvent::CtrlC), &ctx);
    assert!(effects.contains(&Effect::DenyTool {
        id: tool_id,
        reason: String::new(),
    }));
    execute_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        effects,
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut PendingRunIds::default(),
    )
    .await;
    assert_eq!(
        cmd_rx.try_recv(),
        Ok(AgentCommand::DenyTool {
            id: tool_id,
            reason: String::new(),
        })
    );
}

#[tokio::test]
async fn overlapping_approvals_resume_root_and_preserve_input() {
    for approve_subagent in [true, false] {
        let (mut state, _, cmd_tx, mut cmd_rx) = setup();
        state.mode = TuiMode::Streaming;
        let mut textarea = TextArea::from(vec!["unfinished draft".to_string()]);
        let root_tool_id = uuid::Uuid::new_v4();
        let subagent_tool_id = uuid::Uuid::new_v4();
        let subagent_run_id = uuid::Uuid::new_v4();
        state.start_subagent_run(crate::tui_reducer_state::SubagentRunInfo {
            run_id: subagent_run_id,
            bubble_id: uuid::Uuid::new_v4(),
            agent_type: "reviewer".to_string(),
            task: "review".to_string(),
            label: "Review".to_string(),
        });
        let mut pending_run_ids = PendingRunIds::default();
        // Approvals are shown in request order: the subagent asks first, so its
        // prompt is the head and the root's prompt waits behind it.
        for (id, run_id) in [
            (subagent_tool_id, Some(subagent_run_id)),
            (root_tool_id, None),
        ] {
            let context = textarea_ctx_from(&textarea);
            process_one_update(
                &mut state,
                &mut textarea,
                &cmd_tx,
                &context,
                &UiUpdate::ToolPending {
                    id,
                    name: "bash".to_string(),
                    arguments: serde_json::json!({"command": "echo ready"}),
                    content_sections: vec![],
                    title_arguments: vec![],
                    run_id,
                },
                &mut MouseDrag::default(),
                &mut NoopClipboard,
                &mut pending_run_ids,
            )
            .await;
        }
        let context = textarea_ctx_from(&textarea);
        let effects = presenter::update(
            &mut state,
            TuiEvent::Input(if approve_subagent {
                InputEvent::KeyInput(key_event(KeyCode::Enter))
            } else {
                InputEvent::Escape
            }),
            &context,
        );
        execute_effects(
            &mut state,
            &mut textarea,
            &cmd_tx,
            effects,
            &mut MouseDrag::default(),
            &mut NoopClipboard,
            &mut pending_run_ids,
        )
        .await;
        assert!(matches!(
            cmd_rx.try_recv(),
            Ok(AgentCommand::ForSubagent { run_id, command })
                if run_id == subagent_run_id
                    && if approve_subagent {
                        matches!(*command, AgentCommand::ApproveTool { id } if id == subagent_tool_id)
                    } else {
                        matches!(*command, AgentCommand::DenyTool { id, .. } if id == subagent_tool_id)
                    }
        ));
        assert!(
            matches!(state.focus, Focus::ToolApproval { tool_id, .. } if tool_id == root_tool_id),
            "root approval must remain actionable after approving the subagent: {:?}",
            state.focus
        );
        assert_eq!(
            state.active_view,
            crate::tui_reducer_state::ConversationView::Root
        );
        assert_eq!(state.agent_mode(), crate::AgentMode::ToolPending);
        assert_eq!(textarea.lines(), &[""]);

        let context = textarea_ctx_from(&textarea);
        let effects = presenter::update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Enter))),
            &context,
        );
        execute_effects(
            &mut state,
            &mut textarea,
            &cmd_tx,
            effects,
            &mut MouseDrag::default(),
            &mut NoopClipboard,
            &mut pending_run_ids,
        )
        .await;
        assert_eq!(
            cmd_rx.try_recv(),
            Ok(AgentCommand::ApproveTool { id: root_tool_id })
        );
        assert_eq!(state.focus, Focus::MainInput);
        assert_eq!(textarea.lines(), &["unfinished draft"]);
    }
}

#[tokio::test]
async fn two_subagent_approvals_are_presented_fifo_and_route_to_their_own_runs() {
    let (mut state, mut textarea, cmd_tx, mut cmd_rx) = setup();
    state.mode = TuiMode::Streaming;
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
    let mut pending_run_ids = PendingRunIds::default();
    for (tool_id, run_id) in [
        (first_tool_id, first_run_id),
        (second_tool_id, second_run_id),
    ] {
        let context = textarea_ctx_from(&textarea);
        process_one_update(
            &mut state,
            &mut textarea,
            &cmd_tx,
            &context,
            &UiUpdate::ToolPending {
                id: tool_id,
                name: "bash".to_string(),
                arguments: serde_json::json!({"command": "echo ready"}),
                content_sections: vec![],
                title_arguments: vec![],
                run_id: Some(run_id),
            },
            &mut MouseDrag::default(),
            &mut NoopClipboard,
            &mut pending_run_ids,
        )
        .await;
    }

    assert!(matches!(
        state.focus,
        Focus::ToolApproval { tool_id, .. } if tool_id == first_tool_id
    ));
    assert!(matches!(
        state.active_view,
        crate::tui_reducer_state::ConversationView::Subagent { run_id, .. }
            if run_id == first_run_id
    ));

    let context = textarea_ctx_from(&textarea);
    let effects = presenter::update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Enter))),
        &context,
    );
    execute_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        effects,
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut pending_run_ids,
    )
    .await;
    assert!(matches!(
        cmd_rx.try_recv(),
        Ok(AgentCommand::ForSubagent { run_id, command })
            if run_id == first_run_id
                && matches!(*command, AgentCommand::ApproveTool { id } if id == first_tool_id)
    ));
    assert!(matches!(
        state.focus,
        Focus::ToolApproval { tool_id, .. } if tool_id == second_tool_id
    ));
    assert!(matches!(
        state.active_view,
        crate::tui_reducer_state::ConversationView::Subagent { run_id, .. }
            if run_id == second_run_id
    ));

    let context = textarea_ctx_from(&textarea);
    let effects = presenter::update(&mut state, TuiEvent::Input(InputEvent::Escape), &context);
    execute_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        effects,
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut pending_run_ids,
    )
    .await;
    let second_command = cmd_rx
        .try_recv()
        .expect("the second approval verdict should be sent");
    assert!(
        matches!(
            &second_command,
            AgentCommand::ForSubagent { run_id, command }
                if *run_id == second_run_id
                    && matches!(**command, AgentCommand::DenyTool { id, .. } if id == second_tool_id)
        ),
        "second verdict must target the second subagent and tool, got: {second_command:?}"
    );
    assert!(state.pending_approval_order.is_empty());
}

#[tokio::test]
async fn approval_enter_sends_approve_tool() {
    let (mut state, mut textarea, cmd_tx, mut cmd_rx) = setup();
    let tool_id = uuid::Uuid::new_v4();
    let setup_effects = presenter::update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolPending {
            id: tool_id,
            name: "read".to_string(),
            arguments: serde_json::json!({}),
            content_sections: vec![],

            title_arguments: vec![],
            run_id: None,
        }),
        &textarea_ctx_from(&textarea),
    );
    execute_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        setup_effects,
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut PendingRunIds::default(),
    )
    .await;

    let ctx = textarea_ctx_from(&textarea);
    let effects = presenter::update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Enter))),
        &ctx,
    );
    assert!(effects.contains(&Effect::ApproveTool { id: tool_id }));
    execute_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        effects,
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut PendingRunIds::default(),
    )
    .await;
    assert!(matches!(state.focus, Focus::MainInput));
    assert_eq!(
        cmd_rx.try_recv(),
        Ok(AgentCommand::ApproveTool { id: tool_id })
    );
}

#[tokio::test]
async fn approval_escape_sends_deny_tool() {
    let (mut state, mut textarea, cmd_tx, mut cmd_rx) = setup();
    let tool_id = uuid::Uuid::new_v4();
    let setup_effects = presenter::update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolPending {
            id: tool_id,
            name: "read".to_string(),
            arguments: serde_json::json!({}),
            content_sections: vec![],

            title_arguments: vec![],
            run_id: None,
        }),
        &textarea_ctx_from(&textarea),
    );
    execute_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        setup_effects,
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut PendingRunIds::default(),
    )
    .await;

    let ctx = textarea_ctx_from(&textarea);
    let effects = presenter::update(&mut state, TuiEvent::Input(InputEvent::Escape), &ctx);
    assert!(effects.contains(&Effect::DenyTool {
        id: tool_id,
        reason: String::new(),
    }));
    execute_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        effects,
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut PendingRunIds::default(),
    )
    .await;
    assert!(matches!(state.focus, Focus::MainInput));
    assert_eq!(
        cmd_rx.try_recv(),
        Ok(AgentCommand::DenyTool {
            id: tool_id,
            reason: String::new(),
        })
    );
}

#[tokio::test]
async fn save_and_restore_textarea_for_approval() {
    let (mut state, _, cmd_tx, _cmd_rx) = setup();
    let mut textarea = TextArea::from(vec!["hello world".to_string()]);
    configure_textarea(&mut textarea);
    let setup_effects = presenter::update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolPending {
            id: uuid::Uuid::new_v4(),
            name: "read".to_string(),
            arguments: serde_json::json!({}),
            content_sections: vec![],

            title_arguments: vec![],
            run_id: None,
        }),
        &textarea_ctx_from(&textarea),
    );
    execute_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        setup_effects,
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut PendingRunIds::default(),
    )
    .await;
    assert_eq!(
        state.pending_input_snapshot,
        Some(InputSnapshot {
            input_lines: Some(vec!["hello world".to_string()]),
            slash_overlay: SlashOverlay::Hidden,
            pasted_blobs: Default::default(),
        })
    );
    assert_eq!(textarea.lines(), &[""]);

    let context = textarea_ctx_from(&textarea);
    let effects = presenter::update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Enter))),
        &context,
    );
    execute_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        effects,
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut PendingRunIds::default(),
    )
    .await;
    assert_eq!(textarea.lines(), &["hello world"]);
    assert_eq!(state.pending_input_snapshot, None);
}

#[tokio::test]
async fn approval_hides_the_suggestion_overlay_and_reopens_it_when_resolved() {
    // Regression (backlog): typing a slash command and then receiving a
    // tool approval. Arming the approval must hide the overlay,
    // the per-action recompute must keep it hidden while the approval is
    // pending, and resolving the approval must bring it back together
    // with the restored draft.
    let (mut state, _, cmd_tx, mut cmd_rx) = setup();
    let mut mouse_drag = MouseDrag::default();
    let mut clipboard = NoopClipboard;
    let mut pending_run_ids = PendingRunIds::default();

    let mut textarea = TextArea::from(vec!["/mo".to_string()]);
    configure_textarea(&mut textarea);
    state.refresh_slash_overlay(&textarea.lines().join("\n"));
    assert!(matches!(
        state.slash_overlay,
        SlashOverlay::Suggestions { .. }
    ));

    let tool_id = Uuid::new_v4();
    let effects = presenter::update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolPending {
            id: tool_id,
            name: "write".to_string(),
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
            run_id: None,
        }),
        &textarea_ctx_from(&textarea),
    );
    execute_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        effects,
        &mut mouse_drag,
        &mut clipboard,
        &mut pending_run_ids,
    )
    .await;
    assert_eq!(state.slash_overlay, SlashOverlay::Hidden);
    assert_eq!(textarea.lines(), &[""]);

    // Typing a denial reason containing `/` must not reopen the overlay
    // while the approval is pending: the Driver recomputes it after every
    // key action.
    let effects = presenter::update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Char('/')))),
        &textarea_ctx_from(&textarea),
    );
    execute_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        effects,
        &mut mouse_drag,
        &mut clipboard,
        &mut pending_run_ids,
    )
    .await;
    state.refresh_slash_overlay(&textarea.lines().join("\n"));
    assert_eq!(
        state.slash_overlay,
        SlashOverlay::Hidden,
        "the recompute must not reopen the overlay during the approval"
    );

    let effects = presenter::update(
        &mut state,
        TuiEvent::Input(InputEvent::Escape),
        &textarea_ctx_from(&textarea),
    );
    execute_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        effects,
        &mut mouse_drag,
        &mut clipboard,
        &mut pending_run_ids,
    )
    .await;
    assert_eq!(
        cmd_rx.try_recv(),
        Ok(AgentCommand::DenyTool {
            id: tool_id,
            reason: String::new(),
        })
    );
    assert_eq!(
        textarea.lines(),
        &["/mo"],
        "the snapshotted draft must be restored after the approval"
    );
    state.refresh_slash_overlay(&textarea.lines().join("\n"));
    assert!(
        matches!(state.slash_overlay, SlashOverlay::Suggestions { .. }),
        "the overlay must reopen from the restored draft, got {:?}",
        state.slash_overlay
    );
}

#[tokio::test]
async fn mouse_approve_restores_the_draft_and_reopens_the_suggestion_overlay() {
    // The key-action path recomputes the overlay after its effects; the
    // mouse path must do the same so approving with the button restores
    // the slash-command draft *and* its suggestion overlay.
    let (mut state, _, cmd_tx, mut cmd_rx) = setup();
    let mut mouse_drag = MouseDrag::default();
    let mut clipboard = NoopClipboard;
    let mut pending_run_ids = PendingRunIds::default();

    let mut textarea = TextArea::from(vec!["/mo".to_string()]);
    configure_textarea(&mut textarea);
    state.refresh_slash_overlay(&textarea.lines().join("\n"));
    assert!(matches!(
        state.slash_overlay,
        SlashOverlay::Suggestions { .. }
    ));

    let tool_id = Uuid::new_v4();
    let effects = presenter::update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolPending {
            id: tool_id,
            name: "write".to_string(),
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
            run_id: None,
        }),
        &textarea_ctx_from(&textarea),
    );
    execute_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        effects,
        &mut mouse_drag,
        &mut clipboard,
        &mut pending_run_ids,
    )
    .await;
    assert_eq!(state.slash_overlay, SlashOverlay::Hidden);

    state.layout.approve_button = ratatui::layout::Rect::new(1, 1, 12, 1);
    let effects = presenter::update(
        &mut state,
        TuiEvent::Mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 2,
            row: 1,
            modifiers: KeyModifiers::NONE,
        }),
        &textarea_ctx_from(&textarea),
    );
    assert_eq!(
        effects,
        vec![
            Effect::ApproveTool { id: tool_id },
            Effect::RestoreInputState
        ]
    );
    execute_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        effects,
        &mut mouse_drag,
        &mut clipboard,
        &mut pending_run_ids,
    )
    .await;
    assert_eq!(
        cmd_rx.try_recv(),
        Ok(AgentCommand::ApproveTool { id: tool_id })
    );
    // Mirrors the Driver's mouse branch.
    state.refresh_slash_overlay(&textarea.lines().join("\n"));
    assert_eq!(textarea.lines(), &["/mo"]);
    assert!(
        matches!(state.slash_overlay, SlashOverlay::Suggestions { .. }),
        "the overlay must reopen from the restored draft, got {:?}",
        state.slash_overlay
    );
}

#[tokio::test]
async fn approval_resolution_restores_the_models_picker_with_its_catalog() {
    // Regression: accepting `/models` opened the picker, then a tool
    // approval arrived. Hiding the overlay for the approval used to
    // destroy the fetched catalog — the draft text alone cannot
    // reproduce it — so resolving the approval fell back to a
    // suggestion list instead of the picker.
    let (mut state, _, cmd_tx, mut cmd_rx) = setup();
    let mut mouse_drag = MouseDrag::default();
    let mut clipboard = NoopClipboard;
    let mut pending_run_ids = PendingRunIds::default();

    let mut textarea = TextArea::from(vec!["/models deep".to_string()]);
    configure_textarea(&mut textarea);
    state.slash_overlay = SlashOverlay::ModelPicker {
        parameters: None,
        choices: vec![ModelChoice {
            label: crate::tui_reducer_state::ChoiceLabel {
                identity: "stub/deepseek-v3".to_string(),
                details: Vec::new(),
            },
            initial_reasoning_effort: None,
            selection: crate::llm::ProviderModelOption {
                context_length_override: None,
                provider_connection: "stub".to_string(),
                model: crate::llm::ModelInfo {
                    id: "deepseek-v3".to_string(),
                    context_length: None,
                    reasoning_efforts: None,
                    reasoning_default: None,
                    input_modalities: None,
                },
                reasoning_effort: None,
            },
            is_active: true,
            engine_name: None,
            catalog_context_length: None,
            selection_target: None,
            selection_preview_error: None,
        }],
        filter: "deep".to_string(),
        selected: 0,
    };

    let tool_id = Uuid::new_v4();
    let effects = presenter::update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolPending {
            id: tool_id,
            name: "write".to_string(),
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
            run_id: None,
        }),
        &textarea_ctx_from(&textarea),
    );
    execute_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        effects,
        &mut mouse_drag,
        &mut clipboard,
        &mut pending_run_ids,
    )
    .await;
    assert_eq!(state.slash_overlay, SlashOverlay::Hidden);
    let parked = state
        .pending_input_snapshot
        .clone()
        .expect("input state must be parked while the approval is pending");
    assert!(
        matches!(parked.slash_overlay, SlashOverlay::ModelPicker { .. }),
        "the picker must be parked, got {:?}",
        parked.slash_overlay
    );
    assert_eq!(parked.input_lines, Some(vec!["/models deep".to_string()]));

    let effects = presenter::update(
        &mut state,
        TuiEvent::Input(InputEvent::Escape),
        &textarea_ctx_from(&textarea),
    );
    execute_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        effects,
        &mut mouse_drag,
        &mut clipboard,
        &mut pending_run_ids,
    )
    .await;
    assert_eq!(
        cmd_rx.try_recv(),
        Ok(AgentCommand::DenyTool {
            id: tool_id,
            reason: String::new(),
        })
    );
    assert_eq!(state.pending_input_snapshot, None);
    assert_eq!(textarea.lines(), &["/models deep"]);
    // The Driver's post-effect recompute reconciles the restored picker
    // with the restored draft (same filter, kept catalog).
    state.refresh_slash_overlay(&textarea.lines().join("\n"));
    let SlashOverlay::ModelPicker {
        choices, filter, ..
    } = &state.slash_overlay
    else {
        panic!(
            "the models picker must come back with the restored draft, got {:?}",
            state.slash_overlay
        );
    };
    assert_eq!(filter, "deep");
    assert_eq!(choices.len(), 1);
}

#[tokio::test]
async fn approval_input_never_interprets_slash_commands() {
    // The approval popup's input is plain text: no suggestion overlay
    // may open while typing a denial reason that starts with `/`, Tab
    // must not complete a command, and Enter must deny with the typed
    // reason instead of running the command.
    let (mut state, _, cmd_tx, mut cmd_rx) = setup();
    let mut mouse_drag = MouseDrag::default();
    let mut clipboard = NoopClipboard;
    let mut pending_run_ids = PendingRunIds::default();

    let mut textarea = TextArea::from(vec!["hello".to_string()]);
    configure_textarea(&mut textarea);

    let tool_id = Uuid::new_v4();
    let effects = presenter::update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolPending {
            id: tool_id,
            name: "write".to_string(),
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
            run_id: None,
        }),
        &textarea_ctx_from(&textarea),
    );
    execute_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        effects,
        &mut mouse_drag,
        &mut clipboard,
        &mut pending_run_ids,
    )
    .await;
    assert_eq!(state.slash_overlay, SlashOverlay::Hidden);

    // Type "/clear" as the denial reason, mirroring the driver loop:
    // key → effects → per-action recompute.
    for character in "/clear".chars() {
        let effects = presenter::update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Char(character)))),
            &textarea_ctx_from(&textarea),
        );
        execute_effects(
            &mut state,
            &mut textarea,
            &cmd_tx,
            effects,
            &mut mouse_drag,
            &mut clipboard,
            &mut pending_run_ids,
        )
        .await;
        state.refresh_slash_overlay(&textarea.lines().join("\n"));
        assert_eq!(
            state.slash_overlay,
            SlashOverlay::Hidden,
            "no suggestions while typing a denial reason containing `/`"
        );
    }
    assert_eq!(textarea.lines(), &["/clear"]);

    // Tab must not complete a slash command in the approval input.
    let effects = presenter::update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Tab))),
        &textarea_ctx_from(&textarea),
    );
    assert_eq!(effects, vec![Effect::TextareaKey(key_event(KeyCode::Tab))]);

    // Enter must deny with the typed reason, not run `/clear`.
    let effects = presenter::update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Enter))),
        &textarea_ctx_from(&textarea),
    );
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::DenyTool { id, reason }, Effect::RestoreInputState]
                if *id == tool_id && reason == "/clear"
        ),
        "Enter must deny with the reason, got {effects:?}"
    );
    execute_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        effects,
        &mut mouse_drag,
        &mut clipboard,
        &mut pending_run_ids,
    )
    .await;
    assert_eq!(
        cmd_rx.try_recv(),
        Ok(AgentCommand::DenyTool {
            id: tool_id,
            reason: "/clear".to_string(),
        })
    );
}

#[test]
fn escape_closes_help_popup_before_denying_pending_approval() {
    let (mut state, textarea, _cmd_tx, _cmd_rx) = setup();
    let tool_id = uuid::Uuid::new_v4();
    state.focus = Focus::HelpPopup;
    // Simulate tool still pending — put focus back after this test would
    // normally set it via ToolPending.
    let ctx = textarea_ctx_from(&textarea);
    let effects = presenter::update(&mut state, TuiEvent::Input(InputEvent::Escape), &ctx);
    // Escape from HelpPopup should close it without emitting DenyTool.
    assert!(matches!(state.focus, Focus::MainInput));
    assert!(
        !effects.iter().any(|e| matches!(e, Effect::DenyTool { .. })),
        "no DenyTool should be emitted when closing help popup"
    );
    let _ = tool_id; // used in setup
}

#[tokio::test]
async fn clear_resets_the_transcript_and_tells_the_agent_to_start_a_new_session() {
    for command in ["/clear", "/reset"] {
        let (mut state, mut textarea, cmd_tx, mut cmd_rx) = setup();
        let engine: Arc<dyn Engine> = StubEngine::with_models(&[], "large");
        state.push_user_bubble("earlier question".to_string(), UserMessageState::Sent);
        let textarea_context = textarea_ctx_from(&textarea);
        presenter::update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ContextUsage {
                estimated_tokens: 38_000,
                context_length: 98_000,
                source: crate::tui_reducer_state::ContextUsageSource::SerializedRequest,
            }),
            &textarea_context,
        );
        assert_eq!(
            state
                .active_context_usage()
                .map(crate::tui_reducer_state::format_context_usage_readout),
            Some("38.0K (39%)".to_string())
        );
        let (slash_effects, _) = split_slash_effects(submit(&mut state, &mut textarea, command));

        execute_slash_effects(&mut state, slash_effects, &engine, &cmd_tx).await;

        assert_eq!(
            state.active_context_usage(),
            None,
            "{command} must discard the previous session's footer readout"
        );
        assert!(matches!(
            state.chat_entries.as_slice(),
            [crate::tui_reducer_state::ChatEntry::Notice { .. }]
        ));
        // The prompt stays recallable with ↑ even though the transcript is gone.
        assert_eq!(state.input_history, vec!["earlier question".to_string()]);
        assert!(matches!(
            cmd_rx.try_recv().expect("a clear command"),
            AgentCommand::ClearContext
        ));
        let textarea_context = textarea_ctx_from(&textarea);
        presenter::update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ContextUsage {
                estimated_tokens: 1_000,
                context_length: 98_000,
                source: crate::tui_reducer_state::ContextUsageSource::SerializedRequest,
            }),
            &textarea_context,
        );
        assert_eq!(
            state
                .active_context_usage()
                .map(crate::tui_reducer_state::format_context_usage_readout),
            Some("1.0K (1%)".to_string()),
            "the new session must still show its own estimate"
        );
    }
}

#[tokio::test]
async fn tui_run_outcome_follows_driver_events_with_the_headless_precedence() {
    let (mut state, mut textarea, cmd_tx, _cmd_rx) = setup();
    let mut pending_run_ids = PendingRunIds::default();
    let call_id = Uuid::nil();

    let textarea_context = textarea_ctx_from(&textarea);
    process_one_update(
        &mut state,
        &mut textarea,
        &cmd_tx,
        &textarea_context,
        &UiUpdate::Error {
            message: "subagent failed".into(),
            run_id: Some(Uuid::nil()),
        },
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut pending_run_ids,
    )
    .await;
    assert_eq!(
        pending_run_ids.run_outcome.exit_code(),
        crate::ExitCode::Success,
        "a subagent error must not change the run outcome"
    );

    execute_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        vec![Effect::DenyTool {
            id: call_id,
            reason: String::new(),
        }],
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut pending_run_ids,
    )
    .await;
    assert_eq!(
        pending_run_ids.run_outcome.exit_code(),
        crate::ExitCode::ApprovalDenied,
        "a user denial is an approval denial, as in headless mode"
    );

    let textarea_context = textarea_ctx_from(&textarea);
    process_one_update(
        &mut state,
        &mut textarea,
        &cmd_tx,
        &textarea_context,
        &UiUpdate::Error {
            message: "provider failed".into(),
            run_id: None,
        },
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut pending_run_ids,
    )
    .await;
    assert_eq!(
        pending_run_ids.run_outcome.exit_code(),
        crate::ExitCode::GeneralError,
        "a root error outranks an approval denial"
    );

    execute_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        vec![Effect::StopAgent],
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut pending_run_ids,
    )
    .await;
    assert_eq!(
        pending_run_ids.run_outcome.exit_code(),
        crate::ExitCode::GeneralError,
        "a stop request must not replace a recorded outcome"
    );

    pending_run_ids.record_stop_signal();
    assert_eq!(
        pending_run_ids.run_outcome.exit_code(),
        crate::ExitCode::Interrupted,
        "an external stop signal during a turn outranks every other outcome"
    );
}

#[tokio::test]
async fn stop_key_alone_keeps_the_run_outcome_successful() {
    let (mut state, mut textarea, cmd_tx, mut cmd_rx) = setup();
    let mut pending_run_ids = PendingRunIds::default();

    execute_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        vec![Effect::StopAgent],
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut pending_run_ids,
    )
    .await;

    assert!(
        matches!(cmd_rx.try_recv(), Ok(AgentCommand::Stop { .. })),
        "the stop key must still stop the agent"
    );
    assert_eq!(
        pending_run_ids.run_outcome.exit_code(),
        crate::ExitCode::Success,
        "Ctrl+C and Esc stop a turn without setting the exit code"
    );
}

#[test]
fn stop_signal_interrupts_the_run_in_every_state() {
    let mut pending_run_ids = PendingRunIds::default();
    pending_run_ids.record_stop_signal();
    assert_eq!(
        pending_run_ids.run_outcome.exit_code(),
        crate::ExitCode::Interrupted,
        "an external stop signal sets 130 regardless of the TUI state it arrives in"
    );
}

#[tokio::test]
async fn new_prompt_starts_a_fresh_run_outcome() {
    let (mut state, mut textarea, cmd_tx, _cmd_rx) = setup();
    let mut pending_run_ids = PendingRunIds::default();
    pending_run_ids.run_outcome.record_approval_denied();
    pending_run_ids.run_outcome.record_root_error();
    pending_run_ids.record_stop_signal();

    execute_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        vec![Effect::SubmitPrompt { steer: false }],
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut pending_run_ids,
    )
    .await;

    assert_eq!(
        pending_run_ids.run_outcome,
        RunOutcome::default(),
        "only the latest turn decides the exit code"
    );
}

#[tokio::test]
async fn steer_keeps_the_running_turn_outcome() {
    let (mut state, mut textarea, cmd_tx, _cmd_rx) = setup();
    let mut pending_run_ids = PendingRunIds::default();
    pending_run_ids.run_outcome.record_approval_denied();

    execute_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        vec![Effect::SubmitPrompt { steer: true }],
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut pending_run_ids,
    )
    .await;

    assert_eq!(
        pending_run_ids.run_outcome.exit_code(),
        crate::ExitCode::ApprovalDenied,
        "a steer joins the running turn, so its outcome must stay"
    );
}
