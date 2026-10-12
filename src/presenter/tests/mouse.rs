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
fn session_picker_mouse_selects_actions_and_rows_without_confirmation() {
    use crate::tui_reducer_state::{
        SessionAction, SessionChoice, SessionPicker, SessionPickerPhase,
    };
    let mut picker = SessionPicker::loading();
    picker.phase = SessionPickerPhase::Ready;
    picker.choices = (0..3)
        .map(|index| SessionChoice {
            session_id: Uuid::from_u128(index),
            title: "title".into(),
            last_used_at: "date".into(),
        })
        .collect();
    let mut state = AppState::new();
    state.slash_overlay = SlashOverlay::SessionPicker(picker);
    state.layout.slash_overlay.content_area = ratatui::layout::Rect::new(0, 4, 40, 3);
    state.layout.slash_overlay.resume_action = ratatui::layout::Rect::new(0, 8, 10, 1);
    state.layout.slash_overlay.clone_action = ratatui::layout::Rect::new(14, 8, 9, 1);
    for (column, row, kind) in [
        (
            15,
            8,
            MouseEventKind::Down(crossterm::event::MouseButton::Left),
        ),
        (
            2,
            6,
            MouseEventKind::Down(crossterm::event::MouseButton::Left),
        ),
        (2, 6, MouseEventKind::ScrollUp),
    ] {
        let mouse = crossterm::event::MouseEvent {
            column,
            row,
            kind,
            modifiers: KeyModifiers::NONE,
        };
        assert!(update(&mut state, TuiEvent::Mouse(mouse), &empty_textarea()).is_empty());
    }
    let SlashOverlay::SessionPicker(picker) = &mut state.slash_overlay else {
        panic!()
    };
    assert_eq!(picker.action, SessionAction::Clone);
    assert_eq!(picker.selected, 1);
    picker.phase = SessionPickerPhase::Preparing;
    let mouse = crossterm::event::MouseEvent {
        column: 2,
        row: 8,
        kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
        modifiers: KeyModifiers::NONE,
    };
    assert!(update(&mut state, TuiEvent::Mouse(mouse), &empty_textarea()).is_empty());
    let SlashOverlay::SessionPicker(picker) = &state.slash_overlay else {
        panic!()
    };
    assert_eq!(picker.action, SessionAction::Clone);
    assert_eq!(picker.selected, 1);
}

#[test]
fn clarify_choice_click_fills_textarea() {
    let mut state = AppState::new();
    state.mode = TuiMode::ClarifyPending {
        id: Uuid::new_v4(),
        question: "Pick?".to_string(),
        context: None,
        choices: Some(vec!["alpha".to_string(), "beta".to_string()]),
    };
    // Banner starts at row 5, width 40.
    // Row 5: "Pick?" (question), row 6: "1. alpha" (choices start at content row 1).
    state.layout.clarify_banner = ratatui::layout::Rect::new(0, 5, 40, 4);

    let mouse_down = crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 5,
        row: 6,
        modifiers: KeyModifiers::NONE,
    };
    let effects_down = update(&mut state, TuiEvent::Mouse(mouse_down), &empty_textarea());
    assert!(effects_down.is_empty(), "mousedown alone must not select");
    assert_eq!(
        state.clarify_banner_state.clarify_choice_mousedown,
        Some((5, 6))
    );

    let mouse_up = crossterm::event::MouseEvent {
        kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
        column: 5,
        row: 6,
        modifiers: KeyModifiers::NONE,
    };
    let effects_up = update(&mut state, TuiEvent::Mouse(mouse_up), &empty_textarea());
    assert_eq!(
        effects_up,
        vec![Effect::SetInputText("alpha".to_string())],
        "mouseup at same position must fill textarea with choice text"
    );
    assert!(state
        .clarify_banner_state
        .clarify_choice_mousedown
        .is_none());
}

#[test]
fn clarify_choice_click_on_blank_area_does_not_select() {
    let mut state = AppState::new();
    state.mode = TuiMode::ClarifyPending {
        id: Uuid::new_v4(),
        question: "Pick?".to_string(),
        context: None,
        choices: Some(vec!["alpha".to_string()]),
    };
    // Banner at x=0, y=5, width=40.
    // "1. alpha" is 8 chars wide; clicking at col 20 is past the text.
    state.layout.clarify_banner = ratatui::layout::Rect::new(0, 5, 40, 3);

    let mouse_down = crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 20,
        row: 6,
        modifiers: KeyModifiers::NONE,
    };
    update(&mut state, TuiEvent::Mouse(mouse_down), &empty_textarea());

    let mouse_up = crossterm::event::MouseEvent {
        kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
        column: 20,
        row: 6,
        modifiers: KeyModifiers::NONE,
    };
    let effects_up = update(&mut state, TuiEvent::Mouse(mouse_up), &empty_textarea());
    assert!(
        effects_up.is_empty(),
        "click past choice text must not select the choice; got {effects_up:?}"
    );
}

#[test]
fn clarify_choice_click_with_context_offsets_correctly() {
    let mut state = AppState::new();
    // Context is 1 row, spacer is 1 row, question is 1 row → choices start at banner.y + 3.
    state.mode = TuiMode::ClarifyPending {
        id: Uuid::new_v4(),
        question: "Pick an animal?".to_string(),
        context: Some("Context line.".to_string()),
        choices: Some(vec!["rhino".to_string(), "panda".to_string()]),
    };
    // banner.y = 5, width = 40 → context row 5, context→question spacer row 6,
    // question row 7, choices at 8 and 9.
    state.layout.clarify_banner = ratatui::layout::Rect::new(0, 5, 40, 7);

    let click_row = 8u16; // first choice row
    let mouse_down = crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 3,
        row: click_row,
        modifiers: KeyModifiers::NONE,
    };
    update(&mut state, TuiEvent::Mouse(mouse_down), &empty_textarea());

    let mouse_up = crossterm::event::MouseEvent {
        kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
        column: 3,
        row: click_row,
        modifiers: KeyModifiers::NONE,
    };
    let effects_up = update(&mut state, TuiEvent::Mouse(mouse_up), &empty_textarea());
    assert_eq!(
        effects_up,
        vec![Effect::SetInputText("rhino".to_string())],
        "click on first choice row with context present must select first choice"
    );
}

#[test]
fn clarify_choice_click_with_scroll_offset_selects_correct_choice() {
    let mut state = AppState::new();
    // context "line one\nline two" → 1 rendered line (soft break joins)
    // context→question spacer = 1 row
    // question "Pick?" → 1 rendered line
    // choices start at content row 3.
    // With scroll_offset=3 the banner top shows the first choice at visual row banner.y.
    state.mode = TuiMode::ClarifyPending {
        id: Uuid::new_v4(),
        question: "Pick?".to_string(),
        context: Some("line one\nline two".to_string()),
        choices: Some(vec!["alpha".to_string(), "beta".to_string()]),
    };
    state.layout.clarify_banner = ratatui::layout::Rect::new(0, 5, 40, 2);
    state.clarify_banner_state.clarify_scroll_offset = 3; // scrolled so first choice is at visual top

    let click_row = 5u16; // banner.y — visually the first choice
    let mouse_down = crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 3,
        row: click_row,
        modifiers: KeyModifiers::NONE,
    };
    update(&mut state, TuiEvent::Mouse(mouse_down), &empty_textarea());

    let mouse_up = crossterm::event::MouseEvent {
        kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
        column: 3,
        row: click_row,
        modifiers: KeyModifiers::NONE,
    };
    let effects_up = update(&mut state, TuiEvent::Mouse(mouse_up), &empty_textarea());
    assert_eq!(
        effects_up,
        vec![Effect::SetInputText("alpha".to_string())],
        "click at visual top of scrolled banner must resolve to first choice"
    );
}

#[test]
fn clarify_choice_drag_copies_text_to_clipboard() {
    let mut state = AppState::new();
    state.mode = TuiMode::ClarifyPending {
        id: Uuid::new_v4(),
        question: "Pick?".to_string(),
        context: None,
        choices: Some(vec!["alpha".to_string(), "beta".to_string()]),
    };
    // Row 5: "Pick?", row 6: "1. alpha", row 7: "2. beta".
    state.layout.clarify_banner = ratatui::layout::Rect::new(0, 5, 40, 4);

    let mouse_down = crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 5,
        row: 6,
        modifiers: KeyModifiers::NONE,
    };
    update(&mut state, TuiEvent::Mouse(mouse_down), &empty_textarea());

    // Drag to a different column — simulates text selection.
    let mouse_drag = crossterm::event::MouseEvent {
        kind: MouseEventKind::Drag(crossterm::event::MouseButton::Left),
        column: 15,
        row: 6,
        modifiers: KeyModifiers::NONE,
    };
    update(&mut state, TuiEvent::Mouse(mouse_drag), &empty_textarea());
    assert_eq!(
        state.clarify_banner_state.clarify_banner_drag_end,
        Some((15, 6))
    );

    let mouse_up = crossterm::event::MouseEvent {
        kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
        column: 15,
        row: 6,
        modifiers: KeyModifiers::NONE,
    };
    let effects_up = update(&mut state, TuiEvent::Mouse(mouse_up), &empty_textarea());
    assert!(
        effects_up.contains(&Effect::CopyToClipboard("pha".to_string())),
        "drag must copy selected banner text (trimmed to dragged column range) to clipboard"
    );
    assert!(state
        .clarify_banner_state
        .clarify_choice_mousedown
        .is_none());
    assert!(state.clarify_banner_state.clarify_banner_drag_end.is_none());
}

#[test]
fn clarify_choice_multiline_drag_copies_trimmed_text() {
    let mut state = AppState::new();
    state.mode = TuiMode::ClarifyPending {
        id: Uuid::new_v4(),
        question: "Pick?".to_string(),
        context: None,
        choices: Some(vec!["alpha".to_string(), "beta".to_string()]),
    };
    // Banner at x=0, y=5, width=40.
    // Row 5: "Pick?" (question), row 6: "1. alpha", row 7: "2. beta"
    state.layout.clarify_banner = ratatui::layout::Rect::new(0, 5, 40, 4);

    // Drag starts at col=4, row=6 ("1. alpha"[4..] = "lpha") and ends
    // at col=7, row=7 ("2. beta"[0..7] = "2. beta").
    let mouse_down = crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 4,
        row: 6,
        modifiers: KeyModifiers::NONE,
    };
    update(&mut state, TuiEvent::Mouse(mouse_down), &empty_textarea());

    let mouse_drag = crossterm::event::MouseEvent {
        kind: MouseEventKind::Drag(crossterm::event::MouseButton::Left),
        column: 7,
        row: 7,
        modifiers: KeyModifiers::NONE,
    };
    update(&mut state, TuiEvent::Mouse(mouse_drag), &empty_textarea());

    let mouse_up = crossterm::event::MouseEvent {
        kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
        column: 7,
        row: 7,
        modifiers: KeyModifiers::NONE,
    };
    let effects_up = update(&mut state, TuiEvent::Mouse(mouse_up), &empty_textarea());
    assert!(
            effects_up.contains(&Effect::CopyToClipboard("lpha\n2. beta".to_string())),
            "multiline drag must copy first line trimmed from start col, last line trimmed to end col; got {effects_up:?}"
        );
}

#[test]
fn mouse_click_on_scrollbar_respects_help_and_slash_overlay_priority() {
    for help_popup in [false, true] {
        let mut state = AppState::new();
        state.conversation.scroll_offset = 50;
        state.conversation.scroll_locked = false;
        state.layout.chat_scrollbar = crate::tui_reducer_state::ChatScrollbarLayout {
            rect: ratatui::layout::Rect::new(79, 3, 1, 7),
            maximum_scroll_offset: 100,
        };
        if help_popup {
            state.focus = Focus::HelpPopup;
            state.layout.help_popup.rect = ratatui::layout::Rect::new(10, 2, 30, 8);
        } else {
            state.slash_overlay = SlashOverlay::Suggestions {
                choices: Vec::new(),
                selected: 0,
            };
            state.layout.slash_overlay.content_area = ratatui::layout::Rect::new(70, 2, 10, 8);
        }
        let effects = update(
            &mut state,
            TuiEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: 79,
                row: 3,
                modifiers: KeyModifiers::NONE,
            }),
            &empty_textarea(),
        );
        assert!(effects.is_empty());
        assert_eq!(state.conversation.scroll_offset, 50);
        assert!(!state.conversation.scroll_locked);
        assert!(!state.conversation.chat_drag_active);
        if help_popup {
            assert_eq!(state.focus, Focus::MainInput);
        } else {
            assert_eq!(state.slash_overlay_drag_start, Some((79, 3)));
        }
    }
}

#[test]
fn mouse_scroll_up_in_chat_area_decrements_scroll_offset() {
    let mut state = AppState::new();
    state.conversation.scroll_offset = 5;
    state.conversation.scroll_locked = false;
    let mouse = crossterm::event::MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 10,
        row: 10,
        modifiers: KeyModifiers::NONE,
    };
    update(&mut state, TuiEvent::Mouse(mouse), &empty_textarea());
    assert_eq!(state.conversation.scroll_offset, 4);
}

#[test]
fn mouse_scroll_over_model_picker_moves_picker_selection_instead_of_chat() {
    let mut state = AppState::new();
    state.slash_overlay = SlashOverlay::ModelPicker {
        parameters: None,
        choices: (0..3)
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
                selection_target: None,
                selection_preview_error: None,
            })
            .collect(),
        filter: String::new(),
        selected: 0,
    };
    state.conversation.scroll_offset = 5;
    state.conversation.scroll_locked = false;
    let mouse = crossterm::event::MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 10,
        row: 10,
        modifiers: KeyModifiers::NONE,
    };

    update(&mut state, TuiEvent::Mouse(mouse), &empty_textarea());

    assert_eq!(
        state
            .selected_model()
            .map(|choice| choice.selection.model.id.as_str()),
        Some("model-1")
    );
    assert_eq!(
        state.conversation.scroll_offset, 5,
        "scrolling the model picker must not move the chat"
    );
}

#[test]
fn mouse_click_on_model_picker_entry_selects_without_switching() {
    let model_choice = |model_id: &str| ModelChoice {
        label: crate::tui_reducer_state::ChoiceLabel {
            identity: format!("stub/{model_id}"),
            details: Vec::new(),
        },
        initial_reasoning_effort: None,
        selection: ProviderModelOption {
            context_length_override: None,
            provider_connection: "stub".to_string(),
            model: crate::llm::ModelInfo {
                id: model_id.to_string(),
                context_length: None,
                reasoning_efforts: None,
                reasoning_default: None,
                input_modalities: None,
            },
            reasoning_effort: None,
        },
        is_active: false,
        engine_name: None,
        selection_target: None,
        selection_preview_error: None,
    };
    let mut state = AppState::new();
    state.slash_overlay = SlashOverlay::ModelPicker {
        parameters: None,
        choices: vec![model_choice("model-0"), model_choice("model-1")],
        filter: String::new(),
        selected: 0,
    };
    // With the input at row 10, the two-row picker renders its entries at
    // rows 7 and 8.
    state.layout.input_area.rect = ratatui::layout::Rect::new(0, 10, 40, 2);
    state.layout.slash_overlay.content_area = ratatui::layout::Rect::new(1, 7, 37, 2);
    let mouse = crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 5,
        row: 8,
        modifiers: KeyModifiers::NONE,
    };

    let effects = update(&mut state, TuiEvent::Mouse(mouse), &empty_textarea());

    assert_eq!(
        state
            .selected_model()
            .map(|choice| choice.selection.model.id.as_str()),
        Some("model-1")
    );
    assert!(
        effects.is_empty(),
        "clicking a model row must select it without confirming the switch: {effects:?}"
    );
}

#[test]
fn slash_overlay_model_drag_copies_the_filtered_model_label() {
    let model_choice = |label: &str, provider_connection: &str, model_id: &str| ModelChoice {
        label: crate::tui_reducer_state::ChoiceLabel {
            identity: label.to_string(),
            details: Vec::new(),
        },
        initial_reasoning_effort: None,
        selection: ProviderModelOption {
            context_length_override: None,
            provider_connection: provider_connection.to_string(),
            model: crate::llm::ModelInfo {
                id: model_id.to_string(),
                context_length: None,
                reasoning_efforts: None,
                reasoning_default: None,
                input_modalities: None,
            },
            reasoning_effort: None,
        },
        is_active: false,
        engine_name: None,
        selection_target: None,
        selection_preview_error: None,
    };
    let expected_text = "provider/provider-model";
    let mut state = AppState::new();
    state.slash_overlay = SlashOverlay::ModelPicker {
        parameters: None,
        choices: vec![
            model_choice("engine/engine-model", "engine", "engine-model"),
            model_choice(expected_text, "provider", "provider-model"),
        ],
        filter: "provider".to_string(),
        selected: 0,
    };
    state.layout.slash_overlay.content_area = ratatui::layout::Rect::new(1, 7, 37, 1);

    update(
        &mut state,
        TuiEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 3,
            row: 7,
            modifiers: KeyModifiers::NONE,
        }),
        &empty_textarea(),
    );
    update(
        &mut state,
        TuiEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            column: 3 + expected_text.chars().count() as u16,
            row: 7,
            modifiers: KeyModifiers::NONE,
        }),
        &empty_textarea(),
    );
    let effects = update(
        &mut state,
        TuiEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
            column: 3 + expected_text.chars().count() as u16,
            row: 7,
            modifiers: KeyModifiers::NONE,
        }),
        &empty_textarea(),
    );

    assert!(
        effects.iter().any(|effect| matches!(
            effect,
            Effect::CopyToClipboard(text) if text == expected_text
        )),
        "dragging a filtered model row must copy its rendered label: {effects:?}"
    );
}

#[test]
fn slash_overlay_drag_emits_copy_to_clipboard_effect() {
    let mut state = AppState::new();
    state.slash_overlay = SlashOverlay::Suggestions {
        choices: vec![crate::slash_command::SLASH_COMMANDS[1]],
        selected: 0,
    };
    state.layout.slash_overlay.content_area = ratatui::layout::Rect::new(1, 7, 37, 1);

    let mouse_down = crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 3,
        row: 7,
        modifiers: KeyModifiers::NONE,
    };
    update(&mut state, TuiEvent::Mouse(mouse_down), &empty_textarea());

    let mouse_drag = crossterm::event::MouseEvent {
        kind: MouseEventKind::Drag(crossterm::event::MouseButton::Left),
        column: 9,
        row: 7,
        modifiers: KeyModifiers::NONE,
    };
    update(&mut state, TuiEvent::Mouse(mouse_drag), &empty_textarea());

    let mouse_up = crossterm::event::MouseEvent {
        kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
        column: 9,
        row: 7,
        modifiers: KeyModifiers::NONE,
    };
    let effects = update(&mut state, TuiEvent::Mouse(mouse_up), &empty_textarea());

    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::CopyToClipboard(text) if text == "/model")),
        "dragging slash-command text must copy the selected range: {effects:?}"
    );
}

#[test]
fn slash_overlay_drag_uses_the_scrolled_content_row() {
    let mut state = AppState::new();
    state.slash_overlay = SlashOverlay::Suggestions {
        choices: crate::slash_command::SLASH_COMMANDS.to_vec(),
        selected: 0,
    };
    state.layout.slash_overlay = crate::tui_reducer_state::SlashOverlayLayout {
        content_area: ratatui::layout::Rect::new(1, 5, 37, 2),
        first_visible_row: 1,
        ..Default::default()
    };

    update(
        &mut state,
        TuiEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 3,
            row: 5,
            modifiers: KeyModifiers::NONE,
        }),
        &empty_textarea(),
    );
    update(
        &mut state,
        TuiEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            column: 9,
            row: 6,
            modifiers: KeyModifiers::NONE,
        }),
        &empty_textarea(),
    );
    let effects = update(
        &mut state,
        TuiEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
            column: 9,
            row: 6,
            modifiers: KeyModifiers::NONE,
        }),
        &empty_textarea(),
    );

    let expected_first_line = format!(
        "{:<8} {}",
        crate::slash_command::SLASH_COMMANDS[1].name,
        crate::slash_command::SLASH_COMMANDS[1].description
    );
    assert!(
        effects.iter().any(|effect| matches!(
            effect,
            Effect::CopyToClipboard(text)
                if text == &format!("{expected_first_line}\n/tools")
        )),
        "a scrolled drag must map to the visible overlay rows: {effects:?}"
    );
}

#[test]
fn mouse_scroll_over_header_bubble_scrolls_the_header_not_the_chat_area() {
    let mut state = AppState::new();
    state.layout.header_bubble = ratatui::layout::Rect::new(0, 0, 40, 4);
    state.child_view.header_bubble_scroll_offset = 2;
    state.conversation.scroll_offset = 5;
    state.conversation.scroll_locked = false;
    let mouse = crossterm::event::MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 10,
        row: 2,
        modifiers: KeyModifiers::NONE,
    };
    update(&mut state, TuiEvent::Mouse(mouse), &empty_textarea());
    assert_eq!(state.child_view.header_bubble_scroll_offset, 3);
    assert_eq!(
        state.conversation.scroll_offset, 5,
        "scroll over the header must not also move the chat area"
    );
}

#[test]
fn click_on_header_bubble_toggles_its_collapsed_state() {
    use crate::tui_reducer_state::ToolCallState;

    let mut state = AppState::new();
    let bubble_id = Uuid::new_v4();
    state.chat_entries.push(ChatEntry::ToolBubble {
        id: bubble_id,
        name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
        state: Box::new(ToolCallState::Running {
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
            progress_arguments: vec![],
        }),
        collapsed: false,
    });
    let run_id = Uuid::new_v4();
    state.start_child_run(ChildRunInfo {
        run_id,
        bubble_id,
        agent_type: "reviewer".to_string(),
        task: "look into flaky test".to_string(),
        label: "Investigate flaky test".to_string(),
    });
    state.switch_to_child_view(run_id);
    state.layout.header_bubble = ratatui::layout::Rect::new(0, 0, 40, 4);
    let mouse_down = crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 5,
        row: 1,
        modifiers: KeyModifiers::NONE,
    };

    update(&mut state, TuiEvent::Mouse(mouse_down), &empty_textarea());
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
        matches!(&state.chat_entries[0], ChatEntry::ToolBubble { collapsed, .. } if *collapsed),
        "click on the header bubble must toggle the real root bubble's collapsed state"
    );
}

#[test]
fn drag_on_header_bubble_copies_text_without_toggling_collapse() {
    use crate::tui_reducer_state::ToolCallState;

    let mut state = AppState::new();
    let bubble_id = Uuid::new_v4();
    state.chat_entries.push(ChatEntry::ToolBubble {
        id: bubble_id,
        name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
        state: Box::new(ToolCallState::Running {
            arguments: serde_json::json!({
                "agent_type": "reviewer",
                "task": "inspect this task"
            }),
            content_sections: vec![],
            title_arguments: vec![
                ("agent_type".to_string(), serde_json::json!("reviewer")),
                ("task".to_string(), serde_json::json!("inspect this task")),
            ],
            progress_arguments: vec![],
        }),
        collapsed: false,
    });
    let run_id = Uuid::new_v4();
    state.start_child_run(ChildRunInfo {
        run_id,
        bubble_id,
        agent_type: "reviewer".to_string(),
        task: "inspect this task".to_string(),
        label: "Inspect task".to_string(),
    });
    state.switch_to_child_view(run_id);
    render_app_state_buffer(&mut state);
    let task_line = state
        .child_view
        .header_rendered_lines
        .iter()
        .position(|line| line.contains("inspect this task"))
        .expect("rendered header must contain the task");
    let task_column = state.child_view.header_rendered_line_prefix_widths[task_line];
    let task_text_column = state.child_view.header_rendered_lines[task_line]
        .find("inspect")
        .expect("rendered header must contain the selected word");
    let task_row = state.layout.header_bubble.y + task_line as u16;
    let selection_start = state.layout.header_bubble.x + (task_column + task_text_column) as u16;
    let selection_end = selection_start + "inspect".chars().count() as u16;

    update(
        &mut state,
        TuiEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: selection_start,
            row: task_row,
            modifiers: KeyModifiers::NONE,
        }),
        &empty_textarea(),
    );
    assert!(
        matches!(&state.chat_entries[0], ChatEntry::ToolBubble { collapsed, .. } if !*collapsed),
        "header mouse-down must not toggle collapse before the drag is resolved"
    );

    update(
        &mut state,
        TuiEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            column: selection_end,
            row: task_row,
            modifiers: KeyModifiers::NONE,
        }),
        &empty_textarea(),
    );
    let rendered = render_app_state_buffer(&mut state);
    assert_eq!(
        rendered
            .cell((selection_start, task_row))
            .expect("selected header cell")
            .style()
            .bg,
        Some(crate::theme::CHAT_SELECTION_BG),
        "the selected header range must be highlighted"
    );
    let effects = update(
        &mut state,
        TuiEvent::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
            column: selection_end,
            row: task_row,
            modifiers: KeyModifiers::NONE,
        }),
        &empty_textarea(),
    );

    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::CopyToClipboard(text) if text == "inspect")),
        "header drag must copy its selected text: {effects:?}"
    );
    assert!(
        matches!(&state.chat_entries[0], ChatEntry::ToolBubble { collapsed, .. } if !*collapsed),
        "a non-empty header drag must not toggle collapse"
    );
    assert!(!state.child_view.header_drag_active);
}

#[test]
fn click_outside_header_bubble_does_not_toggle_it() {
    use crate::tui_reducer_state::ToolCallState;

    let mut state = AppState::new();
    let bubble_id = Uuid::new_v4();
    state.chat_entries.push(ChatEntry::ToolBubble {
        id: bubble_id,
        name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
        state: Box::new(ToolCallState::Running {
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
            progress_arguments: vec![],
        }),
        collapsed: false,
    });
    let run_id = Uuid::new_v4();
    state.start_child_run(ChildRunInfo {
        run_id,
        bubble_id,
        agent_type: "reviewer".to_string(),
        task: "look into flaky test".to_string(),
        label: "Investigate flaky test".to_string(),
    });
    state.switch_to_child_view(run_id);
    state.layout.header_bubble = ratatui::layout::Rect::new(0, 0, 40, 4);
    let mouse_down = crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 5,
        row: 10,
        modifiers: KeyModifiers::NONE,
    };

    update(&mut state, TuiEvent::Mouse(mouse_down), &empty_textarea());

    assert!(
        matches!(&state.chat_entries[0], ChatEntry::ToolBubble { collapsed, .. } if !*collapsed),
        "click below the header bubble must not toggle it"
    );
}

#[test]
fn mouse_click_outside_help_popup_closes_modal() {
    let mut state = AppState::new();
    state.focus = Focus::HelpPopup;
    // layout.help_popup.rect is default (0,0,0,0), so any click is "outside".
    let mouse = crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 50,
        row: 20,
        modifiers: KeyModifiers::NONE,
    };
    update(&mut state, TuiEvent::Mouse(mouse), &empty_textarea());
    assert_eq!(state.focus, Focus::MainInput);
}

#[test]
fn chat_drag_mouse_up_emits_copy_to_clipboard_effect() {
    // The Presenter must emit CopyToClipboard so the Driver can handle the
    // I/O without the test touching the OS clipboard.
    let mut state = AppState::new();
    state.conversation.chat_drag_active = true;
    state.conversation.chat_rendered_lines = vec!["hello world".to_string()];
    state.conversation.chat_selection = Some(ChatSelection {
        anchor_line: 0,
        anchor_col: 0,
        cursor_line: 0,
        cursor_col: 5,
    });
    let mouse = crossterm::event::MouseEvent {
        kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
        column: 5,
        row: 0,
        modifiers: KeyModifiers::NONE,
    };
    let effects = update(&mut state, TuiEvent::Mouse(mouse), &empty_textarea());
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::CopyToClipboard(t) if t == "hello")),
        "expected CopyToClipboard(\"hello\"), got {effects:?}"
    );
    assert!(
        !state.conversation.chat_drag_active,
        "drag should be cleared"
    );
}

#[test]
fn help_popup_drag_mouse_up_emits_copy_to_clipboard_effect() {
    let mut state = AppState::new();
    state.focus = Focus::HelpPopup;
    state.layout.help_popup.rect = ratatui::layout::Rect::new(8, 3, 24, 6);
    state.layout.help_popup.content_area = ratatui::layout::Rect::new(10, 5, 20, 3);
    state.help_popup.help_rendered_lines = vec!["hello world".to_string()];

    let mouse_down = crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 10,
        row: 5,
        modifiers: KeyModifiers::NONE,
    };
    update(&mut state, TuiEvent::Mouse(mouse_down), &empty_textarea());
    let mouse_drag = crossterm::event::MouseEvent {
        kind: MouseEventKind::Drag(crossterm::event::MouseButton::Left),
        column: 15,
        row: 5,
        modifiers: KeyModifiers::NONE,
    };
    update(&mut state, TuiEvent::Mouse(mouse_drag), &empty_textarea());
    let mouse_up = crossterm::event::MouseEvent {
        kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
        column: 15,
        row: 5,
        modifiers: KeyModifiers::NONE,
    };
    let effects = update(&mut state, TuiEvent::Mouse(mouse_up), &empty_textarea());

    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::CopyToClipboard(text) if text == "hello")),
        "expected CopyToClipboard(\"hello\"), got {effects:?}"
    );
    assert!(
        !state.help_popup.help_drag_active,
        "help popup drag should be cleared"
    );
}

#[test]
fn click_on_thinking_bubble_title_row_toggles_collapsed() {
    let mut state = AppState::new();
    state.chat_entries.push(ChatEntry::ThinkingMessage {
        text: "reasoning".to_string(),
        collapsed: true,
        duration: std::time::Duration::from_secs(1),
    });
    // (start_row=0, end_row=2, entry_index=0) — title row is start (no
    // top-padding row separates the bubble start from its title).
    state.conversation.thinking_title_rows = vec![(0, 2, 0)];
    state.conversation.chat_drag_active = true;
    state.conversation.chat_rendered_lines = vec![String::new(), String::new(), String::new()];
    // A zero-width selection at the title row is a plain click, not a drag.
    state.conversation.chat_selection = Some(ChatSelection {
        anchor_line: 0,
        anchor_col: 3,
        cursor_line: 0,
        cursor_col: 3,
    });
    let mouse = crossterm::event::MouseEvent {
        kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
        column: 3,
        row: 0,
        modifiers: KeyModifiers::NONE,
    };
    update(&mut state, TuiEvent::Mouse(mouse), &empty_textarea());

    assert!(matches!(
        &state.chat_entries[0],
        ChatEntry::ThinkingMessage { collapsed, .. } if !*collapsed
    ));
}

#[test]
fn click_on_streaming_thinking_bubble_title_row_toggles_partial_collapsed() {
    let mut state = AppState::new();
    state.turn_buffer.partial_thinking_text = "pondering...".to_string();
    state.turn_buffer.partial_thinking_collapsed = false;
    // (start_row=0, end_row=2, sentinel index) — title row is start (no
    // top-padding row separates the bubble start from its title).
    state.conversation.thinking_title_rows = vec![(
        0,
        2,
        crate::tui_reducer_state::STREAMING_THINKING_ENTRY_INDEX,
    )];
    state.conversation.chat_drag_active = true;
    state.conversation.chat_rendered_lines = vec![String::new(), String::new(), String::new()];
    // A zero-width selection at the title row is a plain click, not a drag.
    state.conversation.chat_selection = Some(ChatSelection {
        anchor_line: 0,
        anchor_col: 3,
        cursor_line: 0,
        cursor_col: 3,
    });
    let mouse = crossterm::event::MouseEvent {
        kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
        column: 3,
        row: 0,
        modifiers: KeyModifiers::NONE,
    };
    update(&mut state, TuiEvent::Mouse(mouse), &empty_textarea());

    assert!(state.turn_buffer.partial_thinking_collapsed);
}

#[test]
fn single_click_on_spawn_bubble_title_toggles_collapsed_not_drill_in() {
    // A single click must behave exactly like every other bubble
    // (collapse/expand) — drilling in requires a second, fast click. The
    // toggle itself is deferred until the double-click window expires
    // (see `BubbleClickWindowExpired`) so bubble layout stays stable
    // while a confirming second click could still arrive.
    let mut state = AppState::new();
    let bubble_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    state.start_child_run(ChildRunInfo {
        run_id,
        bubble_id,
        agent_type: "reviewer".to_string(),
        task: "look into flaky test".to_string(),
        label: "Investigate flaky test".to_string(),
    });
    let mouse = spawn_bubble_click_fixture(&mut state, bubble_id);

    update(&mut state, TuiEvent::Mouse(mouse), &empty_textarea());

    assert_eq!(state.active_view, ConversationView::Root);
    assert!(matches!(
        &state.chat_entries[0],
        ChatEntry::ToolBubble { collapsed, .. } if !*collapsed
    ));
    assert!(state.last_bubble_click.is_some());

    // The window expires with no confirming second click — now the
    // deferred toggle commits.
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::BubbleClickWindowExpired),
        &empty_textarea(),
    );

    assert!(matches!(
        &state.chat_entries[0],
        ChatEntry::ToolBubble { collapsed, .. } if *collapsed
    ));
    assert!(state.last_bubble_click.is_none());
}

#[test]
fn double_click_on_spawn_bubble_title_drills_into_child_view() {
    let mut state = AppState::new();
    let bubble_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    state.start_child_run(ChildRunInfo {
        run_id,
        bubble_id,
        agent_type: "reviewer".to_string(),
        task: "look into flaky test".to_string(),
        label: "Investigate flaky test".to_string(),
    });
    let mouse = spawn_bubble_click_fixture(&mut state, bubble_id);

    // First click: arms double-click detection, defers the toggle.
    update(&mut state, TuiEvent::Mouse(mouse), &empty_textarea());
    // A zero-width selection at the title row is a plain click, not a drag.
    state.conversation.chat_drag_active = true;
    state.conversation.chat_selection = Some(ChatSelection {
        anchor_line: 0,
        anchor_col: 3,
        cursor_line: 0,
        cursor_col: 3,
    });
    // Second click, same bubble, well within the double-click window.
    update(&mut state, TuiEvent::Mouse(mouse), &empty_textarea());

    assert!(matches!(
        state.active_view,
        ConversationView::Child { run_id: r, .. } if r == run_id
    ));
    // The deferred toggle from the first click must not have committed —
    // the bubble was never collapsed, since the pair resolved as a
    // drill-in instead.
    assert!(matches!(
        &state.chat_entries[0],
        ChatEntry::ToolBubble { collapsed, .. } if !*collapsed
    ));
}

#[test]
fn double_click_on_finished_spawn_bubble_still_drills_in_via_child_conversations() {
    // Once the child finishes, `running_child` is cleared but its entry
    // in `child_conversations` survives — the bubble must still be
    // drillable.
    let mut state = AppState::new();
    let bubble_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    state.start_child_run(ChildRunInfo {
        run_id,
        bubble_id,
        agent_type: "reviewer".to_string(),
        task: "look into flaky test".to_string(),
        label: "Investigate flaky test".to_string(),
    });
    state.running_children.clear();
    let mouse = spawn_bubble_click_fixture(&mut state, bubble_id);

    update(&mut state, TuiEvent::Mouse(mouse), &empty_textarea());
    state.conversation.chat_drag_active = true;
    state.conversation.chat_selection = Some(ChatSelection {
        anchor_line: 0,
        anchor_col: 3,
        cursor_line: 0,
        cursor_col: 3,
    });
    update(&mut state, TuiEvent::Mouse(mouse), &empty_textarea());

    assert!(matches!(
        state.active_view,
        ConversationView::Child { run_id: r, .. } if r == run_id
    ));
}

use crossterm::event::MouseButton;

fn overlay_mouse(kind: MouseEventKind, column: u16, row: u16) -> TuiEvent {
    TuiEvent::Mouse(crossterm::event::MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

/// A visible list with a two-row footer below it, as `/models` draws it.
fn state_with_overlay_footer() -> AppState {
    let mut state = AppState::new();
    state.slash_overlay = SlashOverlay::Suggestions {
        choices: crate::slash_command::SLASH_COMMANDS.to_vec(),
        selected: 0,
    };
    state.layout.slash_overlay = crate::tui_reducer_state::SlashOverlayLayout {
        content_area: ratatui::layout::Rect::new(1, 5, 37, 2),
        detail_area: ratatui::layout::Rect::new(1, 10, 37, 2),
        detail_rows: vec![
            "Compaction effect: shared engine".to_string(),
            "second line".to_string(),
        ],
        ..Default::default()
    };
    state
}

#[test]
fn slash_overlay_footer_drag_copies_the_displayed_footer_text() {
    let mut state = state_with_overlay_footer();

    update(
        &mut state,
        overlay_mouse(MouseEventKind::Down(MouseButton::Left), 1, 10),
        &empty_textarea(),
    );
    update(
        &mut state,
        overlay_mouse(MouseEventKind::Drag(MouseButton::Left), 7, 11),
        &empty_textarea(),
    );
    let effects = update(
        &mut state,
        overlay_mouse(MouseEventKind::Up(MouseButton::Left), 7, 11),
        &empty_textarea(),
    );

    assert!(
        effects.iter().any(|effect| matches!(
            effect,
            Effect::CopyToClipboard(text)
                if text == "Compaction effect: shared engine\nsecond"
        )),
        "footer drag must copy the displayed footer text: {effects:?}"
    );
    assert!(
        !state.conversation.chat_drag_active,
        "a footer press must not start a chat selection underneath the popup"
    );
}

#[test]
fn slash_overlay_footer_drag_is_clamped_to_the_footer() {
    let mut state = state_with_overlay_footer();

    update(
        &mut state,
        overlay_mouse(MouseEventKind::Down(MouseButton::Left), 1, 10),
        &empty_textarea(),
    );
    // Dragging up into the list must not extend the footer selection there.
    update(
        &mut state,
        overlay_mouse(MouseEventKind::Drag(MouseButton::Left), 4, 2),
        &empty_textarea(),
    );
    let effects = update(
        &mut state,
        overlay_mouse(MouseEventKind::Up(MouseButton::Left), 4, 2),
        &empty_textarea(),
    );

    assert!(
        effects.iter().any(|effect| matches!(
            effect,
            Effect::CopyToClipboard(text) if text == "Com"
        )),
        "footer selection must stay in the footer: {effects:?}"
    );
}
