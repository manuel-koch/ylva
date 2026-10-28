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

use super::super::input::{load_history_into_textarea, paste_text, scroll_input_at_mouse};
use super::*;
use crate::tui_reducer_state::HistoryDirection;

#[tokio::test]
async fn input_mouse_targets_the_visible_last_line() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::backend::TestBackend;

    for line_count in [3usize, 12] {
        let (mut state, mut textarea, cmd_tx, _cmd_rx) = setup();
        state.input_history = vec![(0..line_count)
            .map(|line| format!("line {line:02}"))
            .collect::<Vec<_>>()
            .join("\n")];
        assert!(load_history_into_textarea(
            HistoryDirection::Up,
            &mut state,
            &mut textarea,
        ));
        textarea.move_cursor(ratatui_textarea::CursorMove::Top);
        textarea.move_cursor(ratatui_textarea::CursorMove::Head);
        let mut terminal = Terminal::new(TestBackend::new(40, 24)).expect("test terminal created");
        let registry = crate::ToolRegistry::default();
        let mut mouse_drag = MouseDrag::default();
        let mut pending_run_ids = PendingRunIds::default();

        terminal
            .draw(|frame| {
                render::render(frame, &mut state, &mut textarea, None, None, &registry);
            })
            .expect("initial input frame");
        for _ in 0..line_count.saturating_sub(state.layout.input_area.rect.height as usize) {
            assert!(scroll_input_at_mouse(
                &state,
                &mut textarea,
                MouseEvent {
                    kind: MouseEventKind::ScrollDown,
                    column: state.layout.input_area.rect.x + 1,
                    row: state.layout.input_area.rect.y,
                    modifiers: KeyModifiers::NONE,
                },
            ));
            terminal
                .draw(|frame| {
                    render::render(frame, &mut state, &mut textarea, None, None, &registry);
                })
                .expect("scrolled input frame");
        }
        let input_area = state.layout.input_area.rect;
        let last_row = input_area.bottom() - 1;
        let expected_scroll_top = line_count.saturating_sub(input_area.height as usize);
        assert_eq!(state.input_scroll_top, expected_scroll_top);
        let last_line = format!("line {:02}", line_count - 1);
        assert_eq!(
            (0..last_line.len())
                .map(|column| terminal.backend().buffer()
                    [(input_area.x + 1 + column as u16, last_row)]
                    .symbol())
                .collect::<String>(),
            last_line,
            "the actual rendered last line must be visible"
        );
        for (kind, column, expected_column) in [
            (MouseEventKind::Down(MouseButton::Left), input_area.x + 1, 0),
            (MouseEventKind::Drag(MouseButton::Left), input_area.x + 8, 7),
            (MouseEventKind::Up(MouseButton::Left), input_area.x + 8, 7),
        ] {
            let effects = presenter::update(
                &mut state,
                TuiEvent::Mouse(MouseEvent {
                    kind,
                    column,
                    row: last_row,
                    modifiers: KeyModifiers::NONE,
                }),
                &textarea_ctx_from(&textarea),
            );
            execute_effects(
                &mut state,
                &mut textarea,
                &cmd_tx,
                effects,
                &mut mouse_drag,
                &mut NoopClipboard,
                &mut pending_run_ids,
            )
            .await;
            assert_eq!(
                textarea.cursor(),
                (line_count - 1, expected_column),
                "{kind:?} must target the rendered last line ({line_count} lines)"
            );
            terminal
                .draw(|frame| {
                    render::render(frame, &mut state, &mut textarea, None, None, &registry);
                })
                .expect("input frame after mouse event");
            assert_eq!(
                state.input_scroll_top, expected_scroll_top,
                "mouse interaction must not jump the viewport upward"
            );
        }
        assert_eq!(textarea.yank_text(), last_line);
        assert_eq!(state.history_index, None);
        execute_effects(
            &mut state,
            &mut textarea,
            &cmd_tx,
            vec![Effect::PlaceCursorAtMousePos {
                column: input_area.x + 4,
                row: last_row,
            }],
            &mut mouse_drag,
            &mut NoopClipboard,
            &mut pending_run_ids,
        )
        .await;
        assert_eq!(textarea.cursor(), (line_count - 1, 3));
        assert_eq!(textarea.selection_range(), None);
    }
}

#[tokio::test]
async fn input_mouse_uses_rendered_wrap_and_character_widths() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::backend::TestBackend;

    for (text, width, target_column) in [
        ("aaaaa-bbbbb-ccccc-ddddd", 12, 12),
        ("a\tbc", 20, 2),
        ("a\u{754c}bc", 20, 2),
        ("abcdefghijklmnopqrst", 12, 15),
        ("first          last", 12, 16),
    ] {
        let (mut state, _initial_textarea, cmd_tx, _cmd_rx) = setup();
        let mut textarea = TextArea::from([text]);
        configure_textarea(&mut textarea);
        let mut terminal =
            Terminal::new(TestBackend::new(width, 30)).expect("test terminal created");
        let registry = crate::ToolRegistry::default();
        terminal
            .draw(|frame| {
                render::render(frame, &mut state, &mut textarea, None, None, &registry);
            })
            .expect("wrapped input frame");
        textarea.move_cursor(ratatui_textarea::CursorMove::Bottom);
        textarea.move_cursor(ratatui_textarea::CursorMove::End);
        assert_eq!(
            state.layout.input_area.rect.height as usize,
            (textarea.screen_cursor().row + 1).min(10),
            "input height must match the renderer for {text:?}"
        );
        textarea.move_cursor(ratatui_textarea::CursorMove::Jump(0, target_column));
        let target = textarea.screen_cursor();
        assert!(target.row < state.layout.input_area.rect.height as usize);
        textarea.move_cursor(ratatui_textarea::CursorMove::Jump(0, 0));
        let input_area = state.layout.input_area.rect;
        let effects = presenter::update(
            &mut state,
            TuiEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: input_area.x + 1 + target.col as u16,
                row: input_area.y + target.row as u16,
                modifiers: KeyModifiers::NONE,
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
        assert_eq!(
            textarea.cursor(),
            (0, target_column as usize),
            "mouse placement must match the renderer for {text:?}"
        );
    }
}

#[test]
fn input_viewport_tracks_replacement_and_resizing() {
    use ratatui::backend::TestBackend;

    let (mut state, _initial_textarea, _cmd_tx, _cmd_rx) = setup();
    let registry = crate::ToolRegistry::default();
    let mut terminal = Terminal::new(TestBackend::new(40, 24)).expect("test terminal created");
    for line_count in [14usize, 10, 3] {
        let mut textarea = TextArea::from((0..line_count).map(|line| format!("line {line}")));
        configure_textarea(&mut textarea);
        textarea.move_cursor(ratatui_textarea::CursorMove::Bottom);
        textarea.move_cursor(ratatui_textarea::CursorMove::End);
        terminal
            .draw(|frame| {
                render::render(frame, &mut state, &mut textarea, None, None, &registry);
            })
            .expect("replacement draft frame");
        assert_eq!(
            state.input_scroll_top,
            line_count.saturating_sub(state.layout.input_area.rect.height as usize),
            "a replacement textarea must not inherit the previous viewport"
        );
    }

    let mut textarea = TextArea::from(["abcdefghijklmnopqrstuvwxyz"]);
    configure_textarea(&mut textarea);
    terminal
        .draw(|frame| {
            render::render(frame, &mut state, &mut textarea, None, None, &registry);
        })
        .expect("wide terminal frame");
    assert_eq!(state.layout.input_area.rect.height, 1);
    terminal.backend_mut().resize(10, 24);
    terminal
        .draw(|frame| {
            render::render(frame, &mut state, &mut textarea, None, None, &registry);
        })
        .expect("narrow terminal frame");
    assert_eq!(state.layout.input_area.rect.height, 3);
    assert_eq!(state.input_scroll_top, 0);
}

#[tokio::test]
async fn modified_enter_inserts_a_newline_into_the_textarea() {
    for modifier in [KeyModifiers::SHIFT, KeyModifiers::ALT] {
        let (mut state, _initial_textarea, cmd_tx, _cmd_rx) = setup();
        let mut textarea = TextArea::from(vec!["first line".to_string()]);
        configure_textarea(&mut textarea);
        textarea.move_cursor(ratatui_textarea::CursorMove::End);
        let key_event = KeyEvent::new(KeyCode::Enter, modifier);
        let effects = presenter::update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event)),
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

        assert_eq!(
            textarea.lines(),
            &["first line", ""],
            "modifier {modifier:?} should insert a newline"
        );
    }
}

#[tokio::test]
async fn legacy_shift_enter_encoding_inserts_a_newline_into_the_textarea() {
    let (mut state, _initial_textarea, cmd_tx, _cmd_rx) = setup();
    let mut textarea = TextArea::from(vec!["first line".to_string()]);
    configure_textarea(&mut textarea);
    textarea.move_cursor(ratatui_textarea::CursorMove::End);

    let action = crate::events::map_event(crossterm::event::Event::Key(KeyEvent::new(
        KeyCode::Char('j'),
        KeyModifiers::CONTROL,
    )))
    .expect("line-feed key event should be mapped");
    let event = app_action_to_tui_event(action).expect("key action should become a TUI event");
    let effects = presenter::update(&mut state, event, &textarea_ctx_from(&textarea));

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

    assert_eq!(textarea.lines(), &["first line", ""]);
}

#[test]
fn idle_ctrl_c_first_press_enters_double_press_mode() {
    let (mut state, textarea, _cmd_tx, _cmd_rx) = setup();
    let ctx = textarea_ctx_from(&textarea);
    let effects = presenter::update(&mut state, TuiEvent::Input(InputEvent::CtrlC), &ctx);
    assert!(
        effects.is_empty(),
        "first press: no effects (hint shown via mode)"
    );
    assert!(
        matches!(state.mode, TuiMode::DoublePressAbort { .. }),
        "mode should be DoublePressAbort after first press"
    );
}

#[tokio::test]
async fn idle_ctrl_c_double_press_quits() {
    let (mut state, mut textarea, cmd_tx, _cmd_rx) = setup();
    let ctx = textarea_ctx_from(&textarea);
    // First press
    presenter::update(&mut state, TuiEvent::Input(InputEvent::CtrlC), &ctx);
    // Second press
    let effects = presenter::update(&mut state, TuiEvent::Input(InputEvent::CtrlC), &ctx);
    assert!(
        effects.contains(&Effect::Quit),
        "second press should emit Quit"
    );
    let outcome = execute_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        effects,
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut PendingRunIds::default(),
    )
    .await;
    assert_eq!(outcome, ActionOutcome::Quit);
}

#[tokio::test]
async fn idle_escape_clears_input_without_quit() {
    let (mut state, _, cmd_tx, _cmd_rx) = setup();
    let mut textarea = TextArea::from(vec!["draft".to_string()]);
    configure_textarea(&mut textarea);
    let ctx = textarea_ctx_from(&textarea);
    let effects = presenter::update(&mut state, TuiEvent::Input(InputEvent::Escape), &ctx);
    assert!(effects.contains(&Effect::ClearTextarea));
    assert!(!effects.contains(&Effect::Quit));
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
    assert_eq!(textarea.lines(), &[""]);
}

#[tokio::test]
async fn busy_escape_with_draft_clears_input_without_stopping() {
    let (mut state, _, cmd_tx, mut cmd_rx) = setup();
    state.mode = TuiMode::Streaming;
    let mut textarea = TextArea::from(vec!["my draft".to_string()]);
    configure_textarea(&mut textarea);
    let ctx = textarea_ctx_from(&textarea);
    let effects = presenter::update(&mut state, TuiEvent::Input(InputEvent::Escape), &ctx);
    assert!(effects.contains(&Effect::ClearTextarea));
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
    assert_eq!(textarea.lines(), &[""]);
    assert!(cmd_rx.try_recv().is_err(), "no Stop should be sent");
}

#[tokio::test]
async fn busy_escape_with_empty_input_triggers_confirm_to_stop() {
    let (mut state, mut textarea, cmd_tx, mut cmd_rx) = setup();
    state.mode = TuiMode::Streaming;
    let ctx = textarea_ctx_from(&textarea);
    // First Esc: enters double-press mode.
    let effects = presenter::update(&mut state, TuiEvent::Input(InputEvent::Escape), &ctx);
    assert!(effects.is_empty());
    assert!(matches!(state.mode, TuiMode::DoublePressAbort { .. }));
    assert!(cmd_rx.try_recv().is_err());
    // Second Esc: sends Stop.
    let effects2 = presenter::update(&mut state, TuiEvent::Input(InputEvent::Escape), &ctx);
    assert!(effects2.contains(&Effect::StopAgent));
    execute_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        effects2,
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut PendingRunIds::default(),
    )
    .await;
    assert!(matches!(cmd_rx.try_recv(), Ok(AgentCommand::Stop { .. })));
}

#[tokio::test]
async fn busy_double_press_ctrl_c_sends_stop() {
    let (mut state, mut textarea, cmd_tx, mut cmd_rx) = setup();
    state.mode = TuiMode::Streaming;
    let ctx = textarea_ctx_from(&textarea);
    presenter::update(&mut state, TuiEvent::Input(InputEvent::CtrlC), &ctx);
    let effects = presenter::update(&mut state, TuiEvent::Input(InputEvent::CtrlC), &ctx);
    assert!(effects.contains(&Effect::StopAgent));
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
    assert!(matches!(cmd_rx.try_recv(), Ok(AgentCommand::Stop { .. })));
    assert!(
        matches!(state.mode, TuiMode::Aborting),
        "mode should be Aborting after stop"
    );
}

#[tokio::test]
async fn busy_ctrl_c_with_draft_clears_input_without_stopping() {
    let (mut state, _, cmd_tx, mut cmd_rx) = setup();
    state.mode = TuiMode::Streaming;
    let mut textarea = TextArea::from(vec!["my draft".to_string()]);
    configure_textarea(&mut textarea);
    let ctx = textarea_ctx_from(&textarea);
    let effects = presenter::update(&mut state, TuiEvent::Input(InputEvent::CtrlC), &ctx);
    assert!(effects.contains(&Effect::ClearTextarea));
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
    assert_eq!(textarea.lines(), &[""]);
    assert!(cmd_rx.try_recv().is_err());
}

#[test]
fn help_toggle_via_show_help_event() {
    let (mut state, textarea, _cmd_tx, _cmd_rx) = setup();
    let ctx = textarea_ctx_from(&textarea);
    presenter::update(&mut state, TuiEvent::Input(InputEvent::ShowHelp), &ctx);
    assert!(matches!(state.focus, Focus::HelpPopup));
    presenter::update(&mut state, TuiEvent::Input(InputEvent::ShowHelp), &ctx);
    assert!(matches!(state.focus, Focus::MainInput));
}

#[test]
fn clear_and_reset_reach_the_same_handler() {
    let (mut state, mut textarea, _cmd_tx, _cmd_rx) = setup();

    let clear_effects = submit(&mut state, &mut textarea, "/clear");
    let reset_effects = submit(&mut state, &mut textarea, "/reset");

    assert!(clear_effects.contains(&Effect::ClearConversation));
    assert_eq!(clear_effects, reset_effects);
}

#[test]
fn reset_is_selectable_from_the_bare_slash_popup_and_clears_the_conversation() {
    let (mut state, _textarea, _cmd_tx, _cmd_rx) = setup();
    state.refresh_slash_overlay("/");
    let SlashOverlay::Suggestions { choices, .. } = &state.slash_overlay else {
        panic!("expected slash-command suggestions");
    };
    let reset_index = choices
        .iter()
        .position(|choice| choice.name == "/reset")
        .expect("the popup must list /reset separately from /clear");
    for _ in 0..reset_index {
        state.move_slash_selection(1);
    }
    let effects = presenter::update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Enter))),
        &TextareaContext {
            is_empty: false,
            text: "/".to_string(),
            cursor_row: 0,
            cursor_row_is_last: true,
        },
    );
    assert_eq!(
        effects,
        vec![Effect::ClearTextarea, Effect::ClearConversation]
    );
    assert!(state.chat_entries.is_empty());
}

#[test]
fn a_command_is_only_recognized_as_the_first_character_of_the_input() {
    let (mut state, mut textarea, _cmd_tx, _cmd_rx) = setup();

    let mid_message = submit(&mut state, &mut textarea, "please run /clear for me");
    assert!(mid_message.contains(&Effect::SubmitPrompt { steer: false }));
    assert!(!mid_message.contains(&Effect::ClearConversation));

    let indented = submit(&mut state, &mut textarea, " /clear");
    assert!(!indented.contains(&Effect::ClearConversation));
}

#[test]
fn an_unknown_slash_word_is_sent_to_the_model_as_ordinary_input() {
    let (mut state, mut textarea, _cmd_tx, _cmd_rx) = setup();

    let effects = submit(&mut state, &mut textarea, "/foo");

    assert!(effects.contains(&Effect::SubmitPrompt { steer: false }));
    assert!(matches!(
        state.chat_entries.last(),
        Some(crate::tui_reducer_state::ChatEntry::UserMessage { text, .. }) if text == "/foo"
    ));
}

#[test]
fn enter_activates_the_selected_partial_command_instead_of_submitting_it() {
    let (mut state, _textarea, _cmd_tx, _cmd_rx) = setup();
    let mut textarea = TextArea::from(vec!["/model".to_string()]);
    configure_textarea(&mut textarea);
    state.refresh_slash_overlay(&textarea.lines().join("\n"));

    let effects = presenter::update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Enter))),
        &textarea_ctx_from(&textarea),
    );

    assert_eq!(
        effects,
        vec![
            Effect::SetInputText("/models ".to_string()),
            Effect::ListModels { filter: None }
        ],
        "Enter should activate the highlighted /models suggestion"
    );
    assert_eq!(state.slash_overlay, SlashOverlay::Hidden);
    assert!(state.chat_entries.is_empty());
}

#[tokio::test]
async fn tab_completes_models_from_the_partial_model_prefix() {
    let (mut state, _textarea, cmd_tx, _cmd_rx) = setup();
    let mut textarea = TextArea::from(vec!["/model".to_string()]);
    configure_textarea(&mut textarea);
    state.refresh_slash_overlay(&textarea.lines().join("\n"));

    let effects = presenter::update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Tab))),
        &textarea_ctx_from(&textarea),
    );
    assert_eq!(effects, vec![Effect::SetInputText("/models".to_string())]);

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
    assert_eq!(textarea.lines(), &["/models"]);
}

#[test]
fn the_suggestion_overlay_tracks_the_partially_typed_command() {
    let mut state = AppState::new();

    state.refresh_slash_overlay("/mo");
    let SlashOverlay::Suggestions { choices, selected } = &state.slash_overlay else {
        panic!("expected suggestions, got {:?}", state.slash_overlay);
    };
    assert_eq!(choices.len(), 1);
    assert_eq!(choices[0].name, "/models");
    assert_eq!(*selected, 0);

    state.refresh_slash_overlay("hello");
    assert_eq!(state.slash_overlay, SlashOverlay::Hidden);
}

#[tokio::test]
async fn escape_while_command_suggestions_are_open_closes_them_and_clears_the_draft() {
    // Regression: Esc used to only hide the suggestion overlay without
    // touching the draft. Since `refresh_slash_overlay` recomputes the
    // overlay from the draft on every action, the still-matching
    // "/models" text made the very same overlay reopen on that same
    // recompute — Esc looked like a no-op from the user's perspective.
    let (mut state, mut textarea, cmd_tx, _cmd_rx) = setup();
    let mut mouse_drag = MouseDrag::default();
    let mut clipboard = NoopClipboard;
    let mut pending_run_ids = PendingRunIds::default();

    // Type "/models" one character at a time through the real driver
    // path: each key both edits the textarea and re-derives the
    // suggestion overlay from the resulting draft.
    for ch in "/models".chars() {
        let ctx = textarea_ctx_from(&textarea);
        let effects = presenter::update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Char(ch)))),
            &ctx,
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
    }

    assert!(
        matches!(state.slash_overlay, SlashOverlay::Suggestions { .. }),
        "expected the suggestion overlay to be open, got {:?}",
        state.slash_overlay
    );
    assert_eq!(textarea.lines().join("\n"), "/models");

    let ctx = textarea_ctx_from(&textarea);
    let effects = presenter::update(&mut state, TuiEvent::Input(InputEvent::Escape), &ctx);
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
        "Esc must close the suggestion overlay and it must stay closed"
    );
    assert_eq!(
        textarea.lines().join("\n"),
        "",
        "Esc must clear the draft so the overlay cannot immediately reopen"
    );
}

#[test]
fn command_suggestions_navigate_with_up_and_down_and_tab_completes_the_selection() {
    let mut state = AppState::new();
    state.refresh_slash_overlay("/");
    let textarea = TextareaContext {
        is_empty: false,
        text: "/".to_string(),
        cursor_row: 0,
        cursor_row_is_last: true,
    };

    let down = presenter::update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Down))),
        &textarea,
    );
    assert!(down.is_empty());
    assert_eq!(
        state.selected_slash_command().map(|command| command.name),
        Some("/models")
    );

    presenter::update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Down))),
        &textarea,
    );
    assert_eq!(
        state.selected_slash_command().map(|command| command.name),
        Some("/compact")
    );

    presenter::update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Up))),
        &textarea,
    );
    assert_eq!(
        state.selected_slash_command().map(|command| command.name),
        Some("/models")
    );

    let completion = presenter::update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Tab))),
        &textarea,
    );
    assert_eq!(
        completion,
        vec![Effect::SetInputText("/models".to_string())]
    );
}

#[test]
fn suggestion_selection_survives_the_driver_per_action_recompute() {
    // Regression: the Driver recomputes the overlay after every key
    // action (see the key branch of the event loop). Rebuilding the
    // suggestions with `selected: 0` on each recompute made ↑/↓ a no-op
    // in the running app — the presenter moved the highlight and the
    // recompute immediately put it back on the first entry. The unit
    // test above misses this because it skips the recompute.
    let mut state = AppState::new();
    state.refresh_slash_overlay("/");
    let textarea = TextareaContext {
        is_empty: false,
        text: "/".to_string(),
        cursor_row: 0,
        cursor_row_is_last: true,
    };
    let highlighted = |state: &AppState| state.selected_slash_command().map(|command| command.name);

    presenter::update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Down))),
        &textarea,
    );
    state.refresh_slash_overlay(&textarea.text);
    assert_eq!(highlighted(&state), Some("/models"));

    presenter::update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Down))),
        &textarea,
    );
    state.refresh_slash_overlay(&textarea.text);
    assert_eq!(highlighted(&state), Some("/compact"));

    presenter::update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Up))),
        &textarea,
    );
    state.refresh_slash_overlay(&textarea.text);
    assert_eq!(highlighted(&state), Some("/models"));
}

#[test]
fn tab_completes_the_only_matching_command_and_is_normal_input_without_suggestions() {
    let mut state = AppState::new();
    state.refresh_slash_overlay("/re");
    let completion = presenter::update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Tab))),
        &TextareaContext {
            is_empty: false,
            text: "/re".to_string(),
            cursor_row: 0,
            cursor_row_is_last: true,
        },
    );
    assert_eq!(completion, vec![Effect::SetInputText("/reset".to_string())]);

    state.slash_overlay = SlashOverlay::Hidden;
    let tab = key_event(KeyCode::Tab);
    let normal_input = presenter::update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(tab)),
        &TextareaContext {
            is_empty: true,
            text: String::new(),
            cursor_row: 0,
            cursor_row_is_last: true,
        },
    );
    assert_eq!(
        normal_input,
        vec![Effect::ExitBrowsing, Effect::TextareaKey(tab)]
    );
}

async fn run_effects(
    state: &mut AppState,
    textarea: &mut TextArea<'static>,
    cmd_tx: &mpsc::Sender<AgentCommand>,
    effects: Vec<Effect>,
) {
    execute_effects(
        state,
        textarea,
        cmd_tx,
        effects,
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut PendingRunIds::default(),
    )
    .await;
}

#[test]
fn huge_paste_completes_quickly_and_is_collapsed_to_a_marker() {
    let (mut state, mut textarea, _cmd_tx, _cmd_rx) = setup();
    let huge = "some pasted line of text\n".repeat(20_000);
    let started = Instant::now();
    paste_text(&mut state, &mut textarea, &huge);
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    assert_eq!(textarea.lines().len(), 1);
    assert!(textarea.lines()[0].starts_with("[Pasted text #1: 20,000 lines, "));
    assert_eq!(
        state.pasted_blobs.expand(&textarea.lines()[0]),
        huge.trim_end()
    );
}

#[test]
fn paste_over_the_limit_is_rejected_with_a_toast() {
    let (mut state, mut textarea, _cmd_tx, _cmd_rx) = setup();
    paste_text(
        &mut state,
        &mut textarea,
        &"a".repeat(crate::tui_driver::input::MAX_PASTE_BYTES + 1),
    );
    assert_eq!(textarea.lines(), &[""]);
    assert!(state.pasted_blobs.is_empty());
    assert!(state.has_active_toast());
}

#[test]
fn paste_at_the_limit_is_accepted() {
    let (mut state, mut textarea, _cmd_tx, _cmd_rx) = setup();
    paste_text(
        &mut state,
        &mut textarea,
        &"a".repeat(crate::tui_driver::input::MAX_PASTE_BYTES),
    );
    assert!(!state.pasted_blobs.is_empty());
    assert!(!state.has_active_toast());
}

#[test]
fn small_paste_is_inserted_verbatim_with_crlf_and_dedent() {
    let (mut state, mut textarea, _cmd_tx, _cmd_rx) = setup();
    paste_text(&mut state, &mut textarea, "  one\r\n    two\r\n  three");
    assert_eq!(textarea.lines(), &["one", "  two", "three"]);
    assert!(state.pasted_blobs.is_empty());
}

#[test]
fn large_paste_at_the_cursor_keeps_surrounding_text() {
    let (mut state, mut textarea, _cmd_tx, _cmd_rx) = setup();
    textarea.insert_str("before ");
    paste_text(&mut state, &mut textarea, &"x".repeat(1_001));
    textarea.insert_str(" after");
    assert_eq!(
        textarea.lines(),
        &["before [Pasted text #1: 1 lines, 1,001 chars] after"]
    );
    let exactly_threshold = "y".repeat(1_000);
    paste_text(&mut state, &mut textarea, &exactly_threshold);
    assert!(textarea.lines()[0].ends_with(&exactly_threshold));
}

#[tokio::test]
async fn submit_expands_markers_dedents_and_edited_markers_stay_plain() {
    let (mut state, mut textarea, cmd_tx, mut cmd_rx) = setup();
    let first = format!("  {}\n  {}", "a".repeat(600), "b".repeat(600));
    paste_text(&mut state, &mut textarea, &first);
    paste_text(&mut state, &mut textarea, &"c".repeat(1_200));
    assert!(textarea.lines()[0].contains("#1:") && textarea.lines()[0].contains("#2:"));

    run_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        vec![Effect::SubmitPrompt { steer: false }],
    )
    .await;
    let expected = format!(
        "{}\n{}{}",
        "a".repeat(600),
        "b".repeat(600),
        "c".repeat(1_200)
    );
    assert_eq!(cmd_rx.try_recv(), Ok(AgentCommand::Prompt(expected)));

    run_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        vec![Effect::ClearTextarea],
    )
    .await;
    assert!(state.pasted_blobs.is_empty());

    paste_text(&mut state, &mut textarea, &"d".repeat(1_200));
    textarea.insert_str("!");
    textarea.move_cursor(ratatui_textarea::CursorMove::Head);
    textarea.move_cursor(ratatui_textarea::CursorMove::Forward);
    textarea.delete_next_char();
    let edited = textarea.lines().join("\n");
    run_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        vec![Effect::SubmitPrompt { steer: false }],
    )
    .await;
    assert_eq!(cmd_rx.try_recv(), Ok(AgentCommand::Prompt(edited)));
}

#[test]
fn history_recall_of_a_huge_entry_loads_verbatim_without_marker() {
    let (mut state, mut textarea, _cmd_tx, _cmd_rx) = setup();
    let huge = "history line\n".repeat(5_000);
    state.input_history = vec![huge.clone()];
    assert!(load_history_into_textarea(
        HistoryDirection::Up,
        &mut state,
        &mut textarea,
    ));
    assert_eq!(textarea.lines().join("\n"), huge);
    assert!(state.pasted_blobs.is_empty());
}

#[test]
fn paste_exits_history_browsing() {
    let (mut state, mut textarea, _cmd_tx, _cmd_rx) = setup();
    state.input_history = vec!["old".to_string()];
    assert!(load_history_into_textarea(
        HistoryDirection::Up,
        &mut state,
        &mut textarea,
    ));
    paste_text(&mut state, &mut textarea, "new");
    assert_eq!(state.history_index, None);
}

#[test]
fn submitted_bubble_and_history_hold_the_expanded_paste() {
    let (mut state, mut textarea, _cmd_tx, _cmd_rx) = setup();
    textarea.insert_str("explain:\n");
    let pasted = "pasted body\n".repeat(200);
    paste_text(&mut state, &mut textarea, &pasted);
    let effects = presenter::update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Enter))),
        &textarea_ctx_from(&textarea),
    );
    assert!(effects.contains(&Effect::SubmitPrompt { steer: false }));
    let expected = format!("explain:\n{}", pasted.trim_end());
    assert_eq!(state.input_history, vec![expected.clone()]);
    assert!(matches!(
        state.chat_entries.last(),
        Some(ChatEntry::UserMessage { text, .. }) if *text == expected
    ));
}
