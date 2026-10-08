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
fn large_completed_result_warm_render_reuses_selection_storage() {
    use ratatui_textarea::TextArea;
    use ylva::tui_reducer_state::{AppState, ResultSection, ToolCallState};
    let source_body = include_str!("../../src/tui_widgets/chat/cache.rs");
    assert!(
        source_body.lines().count() > 500,
        "highlighting fixture must retain a genuinely large Rust source body"
    );
    let mut state = AppState::new();
    state.enable_render_cache();
    state.chat_entries.push(ChatEntry::ToolBubble {
        id: uuid::Uuid::new_v4(),
        name: "read".into(),
        state: Box::new(ToolCallState::Done {
            arguments: serde_json::json!({}),
            content_sections: vec![ylva::agent::tools::Section::Content(
                ylva::agent::tools::ContentSection {
                    label: Some("src/tui_widgets/chat/cache.rs".into()),
                    content: source_body.into(),
                    file_extension: Some("rs".into()),
                    line_number_offset: Some(0),
                },
            )],
            title_arguments: vec![],
            result_section: ResultSection::plain(""),
            summary: vec![],
            duration: Duration::from_secs(1),
        }),
        collapsed: false,
    });
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    let mut textarea = TextArea::default();
    let registry = ToolRegistry::new();
    terminal
        .draw(|frame| {
            ylva::render::render(frame, &mut state, &mut textarea, None, None, &registry);
        })
        .unwrap();
    let selection_storage = state.conversation.chat_rendered_lines.as_ptr();
    let body_builds = state.render_cache.body_layout_builds;
    let first_buffer = terminal.backend().buffer().clone();
    terminal
        .draw(|frame| {
            ylva::render::render(frame, &mut state, &mut textarea, None, None, &registry);
        })
        .unwrap();
    assert_eq!(
        selection_storage,
        state.conversation.chat_rendered_lines.as_ptr()
    );
    assert_eq!(body_builds, state.render_cache.body_layout_builds);
    assert_eq!(&first_buffer, terminal.backend().buffer());
    assert!(state.render_cache.painted_rows <= 40);
    assert!(state
        .conversation
        .chat_rendered_lines
        .iter()
        .any(|line| line.contains("fn draw_cached_chat")));
    assert!(
        state.conversation.chat_rendered_lines.len() > 500,
        "the full highlighted source must remain available for selection/copy"
    );
}

#[test]
fn cached_selection_uses_terminal_columns_for_wide_unicode_and_graphemes() {
    let mut state = ylva::AppState::new();
    state.enable_render_cache();
    state
        .chat_entries
        .push(completed_render_fixture("日本語 e\u{301} ABC".into()));
    render_offline(&mut state, 80, 20);
    let row = state
        .conversation
        .chat_rendered_lines
        .iter()
        .position(|line| line == "日本語 e\u{301} ABC")
        .unwrap();
    let prefix = state.conversation.chat_rendered_line_prefix_widths[row];
    state.conversation.chat_selection = Some(ylva::ChatSelection {
        anchor_line: row,
        anchor_col: prefix,
        cursor_line: row,
        cursor_col: prefix + 6,
    });
    let buffer = render_offline(&mut state, 80, 20);
    assert_eq!(
        state.extract_chat_selection_text().as_deref(),
        Some("日本語")
    );
    assert_ne!(
        buffer[(prefix as u16, row as u16)].bg,
        buffer[((prefix + 6) as u16, row as u16)].bg
    );
    state.conversation.chat_selection = Some(ylva::ChatSelection {
        anchor_line: row,
        anchor_col: prefix + 7,
        cursor_line: row,
        cursor_col: prefix + 8,
    });
    assert_eq!(
        state.extract_chat_selection_text().as_deref(),
        Some("e\u{301}")
    );
}

#[tokio::test]
async fn drag_in_chat_area_selects_text() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

    let provider =
        ylva::MockProvider::new(vec!["hello world".to_string()], Duration::from_millis(5));
    let backend = TestBackend::new(80, 24);
    let terminal = Terminal::new(backend).expect("test terminal");
    let (action_tx, action_rx) = mpsc::channel(64);
    let registry = Arc::new(ToolRegistry::new());

    let run_handle = tokio::spawn({
        let mut terminal = terminal;
        async move {
            let engine = Arc::new(EngineImpl::new(
                Arc::new(provider),
                common::test_engine_config(),
            ));
            let (agent, ui_rx) = AgentLoop::new(engine, registry);
            tui_driver::run_tui(
                &mut terminal,
                agent,
                ui_rx,
                None,
                action_rx,
                &mut NoopClipboard,
            )
            .await
            .expect("run_tui completed")
        }
    });

    // Let the TUI render at least one frame so chat_area is set.
    tokio::time::sleep(Duration::from_millis(30)).await;

    // Simulate a mouse-down over visible wordmark text (row 5).
    send(
        &action_tx,
        AppAction::MouseInput(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 25,
            row: 5,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }),
    )
    .await;

    // Simulate a drag to column 35 on the same row.
    send(
        &action_tx,
        AppAction::MouseInput(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 35,
            row: 5,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }),
    )
    .await;

    // Mouse-up to finalise.
    send(
        &action_tx,
        AppAction::MouseInput(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 35,
            row: 5,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }),
    )
    .await;

    tokio::time::sleep(Duration::from_millis(30)).await;
    drop(action_tx);

    let state = run_handle.await.expect("task joined");
    // The selection should be set and non-empty.
    let sel = state
        .conversation
        .chat_selection
        .expect("chat_selection should be Some after drag");
    assert!(
        !sel.is_empty(),
        "selection should be non-empty after drag: {:?}",
        sel
    );
}

#[test]
fn dragging_across_logo_build_lines_copies_version_and_profile() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::{
        presenter, render, AppState, Effect, TextareaContext, TuiDisplayMetadata, TuiEvent,
        BUILD_INFO,
    };

    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut state = AppState::new();
    state.display_metadata = TuiDisplayMetadata {
        profile_name: "work".to_string(),
        ..TuiDisplayMetadata::default()
    };
    let mut textarea = TextArea::default();

    terminal
        .draw(|frame| {
            render::render(
                frame,
                &mut state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            );
        })
        .expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let rows: Vec<String> = (0..30u16)
        .map(|row| {
            (0..100u16)
                .map(|column| buffer.cell((column, row)).unwrap().symbol().to_string())
                .collect()
        })
        .collect();
    let version_line = format!("v{}  {}", BUILD_INFO.version, BUILD_INFO.git_commit);
    let version_row = rows
        .iter()
        .position(|row| row.contains(&version_line))
        .expect("version line should be rendered");
    let profile_row = rows
        .iter()
        .position(|row| row.contains("profile: work"))
        .expect("profile line should be rendered");
    let version_column = rows[version_row]
        .find(&version_line)
        .expect("version line start should be visible") as u16;
    let profile_end_column = (rows[profile_row]
        .find("profile: work")
        .expect("profile line start should be visible")
        + "profile: work".len()) as u16;

    let textarea_context = TextareaContext {
        is_empty: true,
        text: String::new(),
        cursor_row: 0,
        cursor_row_is_last: true,
    };
    presenter::update(
        &mut state,
        TuiEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: version_column,
            row: version_row as u16,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }),
        &textarea_context,
    );
    presenter::update(
        &mut state,
        TuiEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: profile_end_column,
            row: profile_row as u16,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }),
        &textarea_context,
    );
    let effects = presenter::update(
        &mut state,
        TuiEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: profile_end_column,
            row: profile_row as u16,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }),
        &textarea_context,
    );

    let expected = format!(
        "v{}  {}\nprofile: work",
        BUILD_INFO.version, BUILD_INFO.git_commit
    );
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::CopyToClipboard(text) if text == &expected)),
        "dragging the logo should copy both build lines without alignment padding: {effects:?}"
    );
}

#[test]
fn dragging_scrolled_help_popup_line_copies_rendered_text() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::{presenter, render, AppState, Effect, Focus, TextareaContext, TuiEvent};

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut state = AppState::new();
    state.focus = Focus::HelpPopup;
    let mut textarea = TextArea::default();

    terminal
        .draw(|frame| {
            render::render(
                frame,
                &mut state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            );
        })
        .expect("initial help popup draw");

    let target_line = state
        .help_popup
        .help_rendered_lines
        .iter()
        .position(|line| line == "Help popup")
        .expect("help popup heading should be rendered");
    state.help_popup.help_scroll_offset = target_line;
    terminal
        .draw(|frame| {
            render::render(
                frame,
                &mut state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            );
        })
        .expect("scrolled help popup draw");

    let content_area = state.layout.help_popup.content_area;
    assert_eq!(
        state.help_popup.help_scroll_offset, target_line,
        "the target heading should be visible at the top of the scrolled popup"
    );
    let target_column_end = content_area.x + "Help popup".chars().count() as u16;
    let target_row = content_area.y;
    let textarea_context = TextareaContext {
        is_empty: true,
        text: String::new(),
        cursor_row: 0,
        cursor_row_is_last: true,
    };

    presenter::update(
        &mut state,
        TuiEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: content_area.x,
            row: target_row,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }),
        &textarea_context,
    );
    presenter::update(
        &mut state,
        TuiEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: target_column_end,
            row: target_row,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }),
        &textarea_context,
    );
    let effects = presenter::update(
        &mut state,
        TuiEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: target_column_end,
            row: target_row,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }),
        &textarea_context,
    );

    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::CopyToClipboard(text) if text == "Help popup")),
        "dragging a scrolled help line should copy its rendered text: {effects:?}"
    );
}
