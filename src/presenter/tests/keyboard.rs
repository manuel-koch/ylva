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
use crate::llm::ModelInfo;
use crate::tui_reducer_state::{ChoiceLabel, ModelChoice, PromptButton};

fn picker_choice(connection: &str, model: &str, favorite_rank: Option<usize>) -> ModelChoice {
    let mut choice = ModelChoice {
        label: ChoiceLabel {
            identity: format!("{connection}/{model}"),
            details: Vec::new(),
        },
        initial_reasoning_effort: Some("medium".to_string()),
        selection: ProviderModelOption {
            context_length_override: None,
            provider_connection: connection.to_string(),
            model: ModelInfo {
                id: model.to_string(),
                context_length: Some(65_536),
                reasoning_efforts: Some(vec!["low".to_string(), "medium".to_string()]),
                reasoning_default: Some("medium".to_string()),
                input_modalities: None,
            },
            reasoning_effort: Some("medium".to_string()),
        },
        is_active: true,
        engine_name: None,
        catalog_context_length: Some(65_536),
        selection_target: None,
        selection_preview_error: None,
        favorite_rank,
    };
    choice.refresh_label();
    choice
}

fn model_picker_state(parameters_open: bool) -> AppState {
    let mut state = AppState::new();
    let mut choices = vec![picker_choice("connection", "first", None)];
    if parameters_open {
        state.slash_overlay = SlashOverlay::ModelPicker {
            parameters: None,
            choices,
            filter: String::new(),
            selected: 0,
        };
        state.open_model_parameters();
    } else {
        choices.push(picker_choice("connection", "second", None));
        state.slash_overlay = SlashOverlay::ModelPicker {
            parameters: None,
            choices,
            filter: "connection".to_string(),
            selected: 1,
        };
    }
    state
}

fn key_event_with_modifiers(code: KeyCode, modifiers: KeyModifiers) -> crossterm::event::KeyEvent {
    crossterm::event::KeyEvent::new(code, modifiers)
}

#[test]
fn session_and_compact_commands_never_submit_prompts_in_either_view() {
    for subagent in [false, true] {
        for command in ["/session", "/compact"] {
            for suggestions in [false, true] {
                let mut state = AppState::new();
                if subagent {
                    let run_id = tracked_subagent(&mut state);
                    state.switch_to_subagent_view(run_id);
                }
                let textarea = TextareaContext {
                    text: if suggestions {
                        command[..3].into()
                    } else {
                        format!("{command} \n")
                    },
                    ..empty_textarea()
                };
                if suggestions {
                    state.refresh_slash_overlay(&textarea.text);
                }
                let effects = update(
                    &mut state,
                    TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Enter))),
                    &textarea,
                );
                assert!(!effects
                    .iter()
                    .any(|effect| matches!(effect, Effect::SubmitPrompt { .. })));
                if command == "/session" {
                    assert!(effects.contains(&Effect::ListSessions));
                } else if subagent {
                    assert!(!effects.contains(&Effect::Compact));
                    assert!(state.chat_entries.iter().any(|entry| matches!(entry,
                        ChatEntry::Notice { text } if text.contains("only in the root"))));
                } else {
                    assert!(effects.contains(&Effect::Compact));
                }
                assert!(state.queued_steer_texts.is_empty());
            }
        }
    }
}

#[test]
fn empty_session_picker_enter_does_not_dispatch_and_escape_cancels_loading() {
    use crate::tui_reducer_state::{SessionPicker, SessionPickerPhase};
    for phase in [
        SessionPickerPhase::Loading,
        SessionPickerPhase::Ready,
        SessionPickerPhase::Preparing,
    ] {
        let mut picker = SessionPicker::loading();
        picker.phase = phase;
        let mut state = AppState::new();
        state.slash_overlay = SlashOverlay::SessionPicker(picker);
        assert!(update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Enter))),
            &empty_textarea()
        )
        .is_empty());
        assert_eq!(
            update(
                &mut state,
                TuiEvent::Input(InputEvent::Escape),
                &empty_textarea()
            ),
            vec![Effect::CancelSessionSelection, Effect::ClearTextarea]
        );
        assert_eq!(state.slash_overlay, SlashOverlay::Hidden);
    }
}

#[test]
fn session_picker_owns_keyboard_action_selection_confirmation_and_cancellation() {
    use crate::tui_reducer_state::{
        SessionAction, SessionChoice, SessionPicker, SessionPickerPhase,
    };
    let mut picker = SessionPicker::loading();
    let source_id = Uuid::new_v4();
    picker.choices = vec![SessionChoice {
        session_id: source_id,
        title: "Untitled".into(),
        last_used_at: "date".into(),
    }];
    let mut state = AppState::new();
    state.slash_overlay = SlashOverlay::SessionPicker(picker);
    for event in [
        InputEvent::Paste("ordinary prompt".into()),
        InputEvent::KeyInput(key_event(KeyCode::Enter)),
    ] {
        assert!(update(&mut state, TuiEvent::Input(event), &empty_textarea()).is_empty());
    }
    let SlashOverlay::SessionPicker(picker) = &mut state.slash_overlay else {
        panic!()
    };
    picker.phase = SessionPickerPhase::Ready;
    for code in [
        KeyCode::Right,
        KeyCode::Down,
        KeyCode::PageDown,
        KeyCode::Char('x'),
    ] {
        assert!(update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(code))),
            &empty_textarea()
        )
        .is_empty());
    }
    assert_eq!(
        update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Enter))),
            &empty_textarea()
        ),
        vec![Effect::SelectSession {
            source_id,
            action: SessionAction::Clone
        }]
    );
    assert_eq!(
        update(
            &mut state,
            TuiEvent::Input(InputEvent::Escape),
            &empty_textarea()
        ),
        vec![Effect::CancelSessionSelection, Effect::ClearTextarea]
    );
    assert_eq!(state.slash_overlay, SlashOverlay::Hidden);
    assert_eq!(SessionPicker::loading().action, SessionAction::Resume);
}

#[test]
fn control_f_toggles_the_filtered_model_pair_and_preserves_input_state() {
    let mut state = model_picker_state(false);
    let textarea = textarea_with("/models connection");
    let effects = update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event_with_modifiers(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL,
        ))),
        &textarea,
    );
    assert_eq!(
        effects,
        vec![Effect::SetFavoriteModel {
            provider_connection: "connection".to_string(),
            model_id: "second".to_string(),
            favorite: true,
        }]
    );
    assert_eq!(textarea.text, "/models connection");
    assert_eq!(state.selected_model().unwrap().selection.model.id, "second");
    assert_eq!(state.engine_display.model, "");
    assert!(state.queued_steer_texts.is_empty());
}

#[test]
fn control_f_toggles_the_parameter_popup_choice_without_editing_its_draft() {
    let mut state = model_picker_state(true);
    let SlashOverlay::ModelPicker {
        parameters: Some(edit_before),
        ..
    } = &state.slash_overlay
    else {
        panic!("expected parameter popup");
    };
    let edit_before = edit_before.clone();
    let effects = update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event_with_modifiers(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL,
        ))),
        &textarea_with("/models first"),
    );
    assert_eq!(
        effects,
        vec![Effect::SetFavoriteModel {
            provider_connection: "connection".to_string(),
            model_id: "first".to_string(),
            favorite: true,
        }]
    );
    let SlashOverlay::ModelPicker {
        parameters: Some(edit_after),
        ..
    } = &state.slash_overlay
    else {
        panic!("parameter popup should remain open");
    };
    assert_eq!(edit_after, &edit_before);
}

#[test]
fn control_f_requires_the_exact_control_modifier() {
    for (character, modifiers) in [
        ('f', KeyModifiers::CONTROL | KeyModifiers::SHIFT),
        ('f', KeyModifiers::CONTROL | KeyModifiers::ALT),
        ('F', KeyModifiers::CONTROL),
        ('f', KeyModifiers::NONE),
        ('F', KeyModifiers::NONE),
    ] {
        let mut state = model_picker_state(false);
        let effects = update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event_with_modifiers(
                KeyCode::Char(character),
                modifiers,
            ))),
            &textarea_with("/models connection"),
        );
        assert!(!effects
            .iter()
            .any(|effect| matches!(effect, Effect::SetFavoriteModel { .. })));
        assert!(!effects.contains(&Effect::SubmitPrompt { steer: false }));
    }
}

#[test]
fn control_f_is_consumed_when_no_model_matches() {
    let mut state = model_picker_state(false);
    if let SlashOverlay::ModelPicker {
        filter, selected, ..
    } = &mut state.slash_overlay
    {
        *filter = "missing".to_string();
        *selected = 0;
    }
    let effects = update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event_with_modifiers(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL,
        ))),
        &textarea_with("/models missing"),
    );
    assert!(effects.is_empty());
    assert!(matches!(
        state.slash_overlay,
        SlashOverlay::ModelPicker { .. }
    ));
}

#[test]
fn control_f_remains_owned_by_help_approval_and_clarify_modes() {
    let event = TuiEvent::Input(InputEvent::KeyInput(key_event_with_modifiers(
        KeyCode::Char('f'),
        KeyModifiers::CONTROL,
    )));

    let mut help = model_picker_state(false);
    help.focus = Focus::HelpPopup;
    assert!(update(&mut help, event.clone(), &empty_textarea()).is_empty());
    assert!(matches!(
        help.slash_overlay,
        SlashOverlay::ModelPicker { .. }
    ));

    let mut approval = model_picker_state(false);
    approval.focus = Focus::ToolApproval {
        tool_id: Uuid::new_v4(),
        tool_name: "bash".to_string(),
    };
    assert_eq!(
        update(&mut approval, event.clone(), &empty_textarea()),
        vec![Effect::TextareaKey(key_event_with_modifiers(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL,
        ))]
    );
    assert!(matches!(
        approval.slash_overlay,
        SlashOverlay::ModelPicker { .. }
    ));

    let mut clarify = model_picker_state(false);
    clarify.mode = TuiMode::ClarifyPending {
        id: Uuid::new_v4(),
        question: "choose".to_string(),
        context: None,
        choices: None,
    };
    assert_eq!(
        update(&mut clarify, event, &empty_textarea()),
        vec![Effect::TextareaKey(key_event_with_modifiers(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL,
        ))]
    );
    assert!(matches!(
        clarify.slash_overlay,
        SlashOverlay::ModelPicker { .. }
    ));
}

#[test]
fn model_picker_pages_selection_with_page_keys() {
    let mut state = AppState::new();
    state.slash_overlay = SlashOverlay::ModelPicker {
        parameters: None,
        choices: (0..10)
            .map(|index| ModelChoice {
                label: crate::tui_reducer_state::ChoiceLabel {
                    identity: format!("stub/model-{index}"),
                    details: Vec::new(),
                },
                initial_reasoning_effort: None,
                selection: ProviderModelOption {
                    context_length_override: None,
                    provider_connection: "stub".to_string(),
                    model: crate::llm::ModelInfo {
                        id: format!("model-{index}"),
                        context_length: None,
                        reasoning_efforts: None,
                        reasoning_default: None,
                        input_modalities: None,
                    },
                    reasoning_effort: None,
                },
                is_active: false,
                engine_name: None,
                catalog_context_length: None,
                selection_target: None,
                selection_preview_error: None,
                favorite_rank: None,
            })
            .collect(),
        filter: String::new(),
        selected: 5,
    };
    let textarea = empty_textarea();

    update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::PageUp))),
        &textarea,
    );
    assert_eq!(
        state
            .selected_model()
            .map(|choice| choice.selection.model.id.as_str()),
        Some("model-2")
    );

    update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::PageDown))),
        &textarea,
    );
    assert_eq!(
        state
            .selected_model()
            .map(|choice| choice.selection.model.id.as_str()),
        Some("model-5")
    );
}

#[test]
fn manual_navigation_to_subagent_view_prevents_auto_return() {
    // A user who already drilled in manually (e.g. clicked the spawn
    // bubble) should not get yanked back to Root the moment the subagent's
    // pending tool call resolves — only an *auto*-switch re-arms
    // auto-return.
    let mut state = AppState::new();
    state.mode = TuiMode::Streaming;
    let run_id = tracked_subagent(&mut state);
    state.switch_to_subagent_view(run_id);
    assert!(!state.auto_switched_to_subagent);

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
    assert!(matches!(
        state.active_view,
        ConversationView::Subagent { .. }
    ));
    assert!(!state.auto_switched_to_subagent);

    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolDenied { id }),
        &empty_textarea(),
    );
    assert!(
        matches!(state.active_view, ConversationView::Subagent { .. }),
        "manual navigation must not auto-return: {:?}",
        state.active_view
    );
}

#[test]
fn root_tool_pending_while_drilled_into_a_subagent_auto_switches_to_root_and_restores_the_subagent_view_on_resolve(
) {
    // Regression: a root-owned approval must not be stranded behind a
    // subagent's transcript the user happens to be drilled into — it
    // has to pull the view back to Root, then restore the subagent view
    // once the approval resolves.
    let mut state = AppState::new();
    state.mode = TuiMode::Streaming;
    let run_id = tracked_subagent(&mut state);
    state.switch_to_subagent_view(run_id);
    assert!(!state.auto_switched_to_subagent);

    let id = Uuid::new_v4();
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolPending {
            id,
            name: "bash".to_string(),
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
            run_id: None,
        }),
        &empty_textarea(),
    );
    assert_eq!(
        state.active_view,
        ConversationView::Root,
        "expected auto-switch to root for the root-owned approval"
    );
    assert!(state.auto_switched_to_subagent);

    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolDenied { id }),
        &empty_textarea(),
    );
    assert!(
        matches!(state.active_view, ConversationView::Subagent { run_id: r, .. } if r == run_id),
        "expected to return to the previously-viewed subagent, got {:?}",
        state.active_view
    );
    assert!(!state.auto_switched_to_subagent);
}

#[test]
fn ctrl_p_in_subagent_view_navigates_back_to_root_while_esc_does_not() {
    let mut state = AppState::new();
    state.mode = TuiMode::Streaming;
    let run_id = tracked_subagent(&mut state);
    state.switch_to_subagent_view(run_id);

    update(
        &mut state,
        TuiEvent::Input(InputEvent::Escape),
        &empty_textarea(),
    );
    assert!(matches!(
        state.active_view,
        ConversationView::Subagent { .. }
    ));

    let effects = update(
        &mut state,
        TuiEvent::Input(InputEvent::ParentView),
        &empty_textarea(),
    );
    assert_eq!(state.active_view, ConversationView::Root);
    assert!(effects.is_empty(), "{effects:?}");
}

#[test]
fn esc_with_non_empty_draft_in_subagent_view_clears_draft_instead_of_navigating() {
    let mut state = AppState::new();
    let run_id = tracked_subagent(&mut state);
    state.switch_to_subagent_view(run_id);

    let effects = update(
        &mut state,
        TuiEvent::Input(InputEvent::Escape),
        &textarea_with("some draft"),
    );

    assert!(matches!(
        state.active_view,
        ConversationView::Subagent { .. }
    ));
    assert!(matches!(effects.as_slice(), [Effect::ClearTextarea]));
}

#[test]
fn ctrl_c_while_idle_first_press_sets_double_press_abort_mode() {
    let mut state = AppState::new();
    state.mode = TuiMode::Idle;
    let effects = update(
        &mut state,
        TuiEvent::Input(InputEvent::CtrlC),
        &empty_textarea(),
    );
    assert!(
        matches!(state.mode, TuiMode::DoublePressAbort { ref return_to, .. } if matches!(**return_to, TuiMode::Idle)),
        "expected DoublePressAbort{{return_to: Idle}}, got {:?}",
        state.mode
    );
    assert!(!effects.contains(&Effect::Quit));
}

#[test]
fn ctrl_c_while_idle_second_press_returns_quit_effect() {
    let mut state = AppState::new();
    state.mode = TuiMode::DoublePressAbort {
        pressed_at: Instant::now(),
        return_to: Box::new(TuiMode::Idle),
        armed_by: ConfirmKey::CtrlC,
    };
    let effects = update(
        &mut state,
        TuiEvent::Input(InputEvent::CtrlC),
        &empty_textarea(),
    );
    assert!(effects.contains(&Effect::Quit));
}

#[test]
fn escape_after_ctrl_c_while_idle_clears_draft_instead_of_confirming_quit() {
    // Regression: pressing ctrl-c once (arming the quit confirm), then
    // Esc, must not be treated as the confirming second press. Esc
    // dismisses the pending quit and clears the draft instead.
    let mut state = AppState::new();
    state.mode = TuiMode::Idle;
    update(
        &mut state,
        TuiEvent::Input(InputEvent::CtrlC),
        &empty_textarea(),
    );
    assert!(matches!(state.mode, TuiMode::DoublePressAbort { .. }));

    let effects = update(
        &mut state,
        TuiEvent::Input(InputEvent::Escape),
        &empty_textarea(),
    );
    assert!(!effects.contains(&Effect::Quit));
    assert_eq!(state.mode, TuiMode::Idle);
}

#[test]
fn ctrl_c_after_escape_while_streaming_does_not_quit() {
    // Regression: pressing Esc once (arming the cancel-only confirm),
    // then ctrl-c, must not be treated as ctrl-c's confirming second
    // press. ctrl-c instead starts its own fresh confirm window.
    let mut state = AppState::new();
    state.mode = TuiMode::Streaming;
    update(
        &mut state,
        TuiEvent::Input(InputEvent::Escape),
        &empty_textarea(),
    );
    assert!(matches!(
        state.mode,
        TuiMode::DoublePressAbort {
            armed_by: ConfirmKey::Escape,
            ..
        }
    ));

    let effects = update(
        &mut state,
        TuiEvent::Input(InputEvent::CtrlC),
        &empty_textarea(),
    );
    assert!(!effects.contains(&Effect::StopAgent));
    assert!(!state.aborting_exit);
    assert!(matches!(
        state.mode,
        TuiMode::DoublePressAbort {
            armed_by: ConfirmKey::CtrlC,
            ..
        }
    ));
}

#[test]
fn ctrl_c_while_busy_first_press_records_confirm() {
    let mut state = AppState::new();
    state.mode = TuiMode::Streaming;
    let effects = update(
        &mut state,
        TuiEvent::Input(InputEvent::CtrlC),
        &empty_textarea(),
    );
    assert!(
        matches!(state.mode, TuiMode::DoublePressAbort { ref return_to, .. } if matches!(**return_to, TuiMode::Streaming)),
        "expected DoublePressAbort{{return_to: Streaming}}"
    );
    assert!(!effects.contains(&Effect::StopAgent));
}

#[test]
fn ctrl_c_while_busy_second_press_returns_stop_agent_effect() {
    let mut state = AppState::new();
    state.mode = TuiMode::DoublePressAbort {
        pressed_at: Instant::now(),
        return_to: Box::new(TuiMode::Streaming),
        armed_by: ConfirmKey::CtrlC,
    };
    let effects = update(
        &mut state,
        TuiEvent::Input(InputEvent::CtrlC),
        &empty_textarea(),
    );
    assert!(effects.contains(&Effect::StopAgent));
    assert_eq!(state.mode, TuiMode::Aborting);
    assert!(
        state.aborting_exit,
        "aborting_exit must be set after double-press ctrl-c while busy"
    );
}

#[test]
fn double_ctrl_c_while_streaming_quits_after_stream_cancelled() {
    let mut state = AppState::new();
    // First ctrl-c: enters DoublePressAbort.
    state.mode = TuiMode::Streaming;
    update(
        &mut state,
        TuiEvent::Input(InputEvent::CtrlC),
        &empty_textarea(),
    );
    // Second ctrl-c: sends StopAgent and sets aborting_exit.
    let effects = update(
        &mut state,
        TuiEvent::Input(InputEvent::CtrlC),
        &empty_textarea(),
    );
    assert!(effects.contains(&Effect::StopAgent));
    assert!(state.aborting_exit);
    // StreamCancelled arrives: must emit Quit.
    let effects = update(
        &mut state,
        TuiEvent::Domain(DomainEvent::StreamCancelled),
        &empty_textarea(),
    );
    assert!(
        effects.contains(&Effect::Quit),
        "StreamCancelled after double ctrl-c while streaming must emit Quit"
    );
}

#[test]
fn double_ctrl_c_while_tool_running_quits_after_tool_killed() {
    let mut state = AppState::new();
    state.mode = TuiMode::Streaming;
    // First ctrl-c.
    update(
        &mut state,
        TuiEvent::Input(InputEvent::CtrlC),
        &empty_textarea(),
    );
    // Second ctrl-c: stop agent, set aborting_exit.
    let effects = update(
        &mut state,
        TuiEvent::Input(InputEvent::CtrlC),
        &empty_textarea(),
    );
    assert!(effects.contains(&Effect::StopAgent));
    assert!(state.aborting_exit);
    // ToolKilled arrives (no StreamCancelled because no HTTP stream was active).
    let effects = update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolKilled {
            id: Uuid::new_v4(),
            run_id: None,
        }),
        &empty_textarea(),
    );
    assert!(
        effects.contains(&Effect::Quit),
        "ToolKilled after double ctrl-c while tool running must emit Quit"
    );
    assert_eq!(state.mode, TuiMode::Idle, "mode must return to Idle");
}

#[test]
fn ctrl_c_while_tool_approval_denies_tool() {
    let mut state = AppState::new();
    let tool_id = Uuid::new_v4();
    state.focus = Focus::ToolApproval {
        tool_id,
        tool_name: "test_tool".to_string(),
    };
    let effects = update(
        &mut state,
        TuiEvent::Input(InputEvent::CtrlC),
        &empty_textarea(),
    );
    assert!(effects.contains(&Effect::DenyTool {
        id: tool_id,
        reason: String::new(),
    }));
    assert!(effects.contains(&Effect::RestoreSlashOverlay));
    assert_eq!(state.focus, Focus::MainInput);
}

#[test]
fn escape_while_help_open_closes_help() {
    let mut state = AppState::new();
    state.focus = Focus::HelpPopup;
    update(
        &mut state,
        TuiEvent::Input(InputEvent::Escape),
        &empty_textarea(),
    );
    assert_eq!(state.focus, Focus::MainInput);
}

#[test]
fn escape_while_tool_approval_only_drops_button_focus() {
    let mut state = AppState::new();
    let tool_id = Uuid::new_v4();
    state.focus = Focus::ToolApproval {
        tool_id,
        tool_name: "test_tool".to_string(),
    };
    state.prompt_button_focus = Some(PromptButton::Deny);
    let effects = update(
        &mut state,
        TuiEvent::Input(InputEvent::Escape),
        &empty_textarea(),
    );
    assert!(effects.is_empty());
    assert_eq!(state.prompt_button_focus, None);
    assert!(matches!(state.focus, Focus::ToolApproval { .. }));
}

#[test]
fn escape_while_busy_empty_input_triggers_confirm() {
    let mut state = AppState::new();
    state.mode = TuiMode::Streaming;
    update(
        &mut state,
        TuiEvent::Input(InputEvent::Escape),
        &empty_textarea(),
    );
    assert!(
        matches!(state.mode, TuiMode::DoublePressAbort { .. }),
        "expected DoublePressAbort after first Escape"
    );
    // Second press → stop agent but stay in TUI (mode should be Aborting
    // for visual feedback, but aborting_exit must stay false).
    let effects = update(
        &mut state,
        TuiEvent::Input(InputEvent::Escape),
        &empty_textarea(),
    );
    assert!(effects.contains(&Effect::StopAgent));
    assert!(
        matches!(state.mode, TuiMode::Aborting),
        "Esc second press must set Aborting for visual feedback"
    );
    assert!(
        !state.aborting_exit,
        "Esc second press must not set aborting_exit — TUI must stay open"
    );
}

#[test]
fn show_help_toggles_focus_to_help_popup() {
    let mut state = AppState::new();
    state.focus = Focus::MainInput;
    update(
        &mut state,
        TuiEvent::Input(InputEvent::ShowHelp),
        &empty_textarea(),
    );
    assert_eq!(state.focus, Focus::HelpPopup);
    update(
        &mut state,
        TuiEvent::Input(InputEvent::ShowHelp),
        &empty_textarea(),
    );
    assert_eq!(state.focus, Focus::MainInput);
}

#[test]
fn stop_signal_quits_when_idle_in_every_focus() {
    for focus in [Focus::MainInput, Focus::HelpPopup] {
        let mut state = AppState::new();
        state.focus = focus.clone();
        let effects = update(
            &mut state,
            TuiEvent::Input(InputEvent::StopSignal),
            &empty_textarea(),
        );
        assert_eq!(effects, vec![Effect::Quit], "focus {focus:?}");
        assert_eq!(state.mode, TuiMode::Idle, "focus {focus:?}");
    }
}

#[test]
fn stop_signal_stops_busy_agent_in_every_focus() {
    let tool_id = Uuid::new_v4();
    let busy_cases = [
        (TuiMode::Pending, Focus::MainInput),
        (TuiMode::Streaming, Focus::MainInput),
        (TuiMode::Streaming, Focus::HelpPopup),
        (
            TuiMode::Streaming,
            Focus::ToolApproval {
                tool_id,
                tool_name: "shell".to_string(),
            },
        ),
        (
            TuiMode::ClarifyPending {
                id: Uuid::new_v4(),
                question: "which?".to_string(),
                context: None,
                choices: None,
            },
            Focus::MainInput,
        ),
    ];
    for (mode, focus) in busy_cases {
        let mut state = AppState::new();
        state.mode = mode.clone();
        state.focus = focus.clone();
        let effects = update(
            &mut state,
            TuiEvent::Input(InputEvent::StopSignal),
            &empty_textarea(),
        );
        assert_eq!(effects, vec![Effect::StopAgent], "{mode:?} / {focus:?}");
        assert_eq!(state.mode, TuiMode::Aborting, "{mode:?} / {focus:?}");
        assert!(state.aborting_exit, "{mode:?} / {focus:?}");
    }
}

#[test]
fn stop_signal_stops_idle_root_while_subagent_still_runs() {
    let mut state = AppState::new();
    state.chat_entries.push(ChatEntry::ToolBubble {
        id: Uuid::new_v4(),
        name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
        state: Box::new(ToolCallState::Running {
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
            progress_arguments: vec![],
        }),
        collapsed: false,
    });
    assert_eq!(state.mode, TuiMode::Idle);
    let effects = update(
        &mut state,
        TuiEvent::Input(InputEvent::StopSignal),
        &empty_textarea(),
    );
    assert_eq!(effects, vec![Effect::StopAgent]);
    assert_eq!(state.mode, TuiMode::Aborting);
    assert!(state.aborting_exit);
}

#[test]
fn enter_with_text_returns_submit_prompt_effect() {
    let mut state = AppState::new();
    let effects = update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Enter))),
        &textarea_with("hello"),
    );
    assert!(effects.contains(&Effect::SubmitPrompt { steer: false }));
    assert!(effects.contains(&Effect::ClearTextarea));
}

#[test]
fn enter_with_text_in_subagent_view_is_a_no_op() {
    // Regression test: Enter must not silently steer the *root* agent
    // while the user is looking at a subagent's drill-in transcript —
    // there is no visual confirmation of which conversation the message
    // would go to, and the subagent conversation itself does not accept
    // input.
    let mut state = AppState::new();
    state.mode = TuiMode::Streaming;
    let run_id = tracked_subagent(&mut state);
    state.switch_to_subagent_view(run_id);

    let effects = update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Enter))),
        &textarea_with("hello"),
    );

    assert!(effects.is_empty(), "expected no effects, got {effects:?}");
    assert!(
        state.queued_steer_texts.is_empty(),
        "must not queue a steer to the root agent"
    );
    assert_eq!(
        state.mode,
        TuiMode::Streaming,
        "must not touch the root agent's TuiMode"
    );
}

#[test]
fn enter_while_approval_without_button_focus_does_nothing() {
    let mut state = AppState::new();
    state.focus = Focus::ToolApproval {
        tool_id: Uuid::new_v4(),
        tool_name: "test_tool".to_string(),
    };
    for text in ["", "deny reason"] {
        let effects = update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Enter))),
            &textarea_with(text),
        );
        assert!(effects.is_empty(), "{effects:?}");
        assert!(matches!(state.focus, Focus::ToolApproval { .. }));
    }
}

#[test]
fn tab_cycles_approval_buttons_and_enter_activates_the_focused_one() {
    let mut state = AppState::new();
    let tool_id = Uuid::new_v4();
    state.focus = Focus::ToolApproval {
        tool_id,
        tool_name: "test_tool".to_string(),
    };
    let tab = TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Tab)));
    update(&mut state, tab.clone(), &empty_textarea());
    assert_eq!(state.prompt_button_focus, Some(PromptButton::Approve));
    update(&mut state, tab.clone(), &empty_textarea());
    assert_eq!(state.prompt_button_focus, Some(PromptButton::Deny));
    update(&mut state, tab, &empty_textarea());
    assert_eq!(state.prompt_button_focus, Some(PromptButton::Approve));

    // Approve leaves the typed text alone.
    let effects = update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Enter))),
        &textarea_with("ignored"),
    );
    assert_eq!(
        effects,
        vec![
            Effect::ApproveTool { id: tool_id },
            Effect::RestoreSlashOverlay
        ]
    );
}

#[test]
fn enter_on_focused_deny_uses_and_clears_the_text() {
    let mut state = AppState::new();
    let tool_id = Uuid::new_v4();
    state.focus = Focus::ToolApproval {
        tool_id,
        tool_name: "test_tool".to_string(),
    };
    state.prompt_button_focus = Some(PromptButton::Deny);
    let effects = update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Enter))),
        &textarea_with("deny reason"),
    );
    assert_eq!(
        effects,
        vec![
            Effect::DenyTool {
                id: tool_id,
                reason: "deny reason".to_string(),
            },
            Effect::ClearTextarea,
            Effect::RestoreSlashOverlay,
        ]
    );
}

#[test]
fn clarify_digit_passes_through_to_textarea() {
    let mut state = AppState::new();
    state.mode = TuiMode::ClarifyPending {
        id: Uuid::new_v4(),
        question: "Pick one".to_string(),
        context: None,
        choices: Some(vec!["alpha".to_string(), "beta".to_string()]),
    };
    let key = key_event(KeyCode::Char('2'));
    let effects = update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key)),
        &empty_textarea(),
    );
    assert_eq!(
        effects,
        vec![Effect::TextareaKey(key_event(KeyCode::Char('2')))]
    );
}

#[test]
fn clarify_enter_answers_only_with_ok_focused() {
    let mut state = AppState::new();
    let clarify_id = Uuid::new_v4();
    state.mode = TuiMode::ClarifyPending {
        id: clarify_id,
        question: "Pick one".to_string(),
        context: None,
        choices: None,
    };
    let enter = TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Enter)));
    let effects = update(
        &mut state,
        enter.clone(),
        &textarea_with("free-form answer"),
    );
    assert!(effects.is_empty());
    assert!(matches!(state.mode, TuiMode::ClarifyPending { .. }));

    update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Tab))),
        &empty_textarea(),
    );
    assert_eq!(state.prompt_button_focus, Some(PromptButton::ClarifyOk));
    let effects = update(&mut state, enter, &textarea_with("free-form answer"));
    assert_eq!(state.mode, TuiMode::Pending);
    assert_eq!(
        effects,
        vec![
            Effect::AnswerClarify {
                id: clarify_id,
                answer: "free-form answer".to_string(),
            },
            Effect::ClearTextarea,
            Effect::RestoreSlashOverlay,
        ]
    );
}

#[test]
fn clarify_cancel_button_aborts_question_but_esc_does_not() {
    let mut state = AppState::new();
    let clarify_id = Uuid::new_v4();
    state.mode = TuiMode::ClarifyPending {
        id: clarify_id,
        question: "Pick one".to_string(),
        context: None,
        choices: None,
    };
    let effects = update(
        &mut state,
        TuiEvent::Input(InputEvent::Escape),
        &empty_textarea(),
    );
    assert!(effects.is_empty());

    state.prompt_button_focus = Some(PromptButton::ClarifyCancel);
    let effects = update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Enter))),
        &empty_textarea(),
    );
    assert_eq!(state.mode, TuiMode::Pending);
    assert!(effects.contains(&Effect::AbortClarify { id: clarify_id }));
    assert!(effects.contains(&Effect::RestoreSlashOverlay));
}

#[test]
fn clarify_esc_with_non_empty_input_clears_input_and_button_focus() {
    let mut state = AppState::new();
    state.mode = TuiMode::ClarifyPending {
        id: Uuid::new_v4(),
        question: "Pick one".to_string(),
        context: None,
        choices: None,
    };
    state.prompt_button_focus = Some(PromptButton::ClarifyOk);
    let effects = update(
        &mut state,
        TuiEvent::Input(InputEvent::Escape),
        &textarea_with("partial answer"),
    );
    assert_eq!(state.prompt_button_focus, None);
    assert!(
        matches!(state.mode, TuiMode::ClarifyPending { .. }),
        "mode must stay ClarifyPending when input is not empty"
    );
    assert_eq!(effects, vec![Effect::ClearTextarea]);
}

#[test]
fn keyboard_in_help_mode_scrolls_help() {
    let mut state = AppState::new();
    state.focus = Focus::HelpPopup;
    state.help_popup.help_scroll_offset = 5;
    update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Down))),
        &empty_textarea(),
    );
    assert_eq!(state.help_popup.help_scroll_offset, 6);
}

#[test]
fn closing_help_during_an_outstanding_approval_returns_to_the_approval_focus() {
    // Regression: closing the help popup used to drop focus to
    // `Focus::MainInput` even while a tool still awaited its verdict —
    // handing the slash-command-capable main input to the user and
    // hiding the approval controls.
    let mut state = AppState::new();
    let tool_id = Uuid::new_v4();
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolPending {
            id: tool_id,
            name: "write".to_string(),
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
            run_id: None,
        }),
        &empty_textarea(),
    );
    assert!(matches!(state.focus, Focus::ToolApproval { .. }));

    let effects = update(
        &mut state,
        TuiEvent::Input(InputEvent::ShowHelp),
        &empty_textarea(),
    );
    assert!(effects.is_empty());
    assert!(matches!(state.focus, Focus::HelpPopup));

    let effects = update(
        &mut state,
        TuiEvent::Input(InputEvent::Escape),
        &empty_textarea(),
    );
    assert_eq!(
        state.focus,
        Focus::ToolApproval {
            tool_id,
            tool_name: "write".to_string()
        },
        "closing help must resume the approval focus while the verdict is outstanding"
    );
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::DenyTool { .. })),
        "closing help must not resolve the approval"
    );
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::RestoreSlashOverlay)),
        "closing help must not restore the input — the pause is still active"
    );
}
