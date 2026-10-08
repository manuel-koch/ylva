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
fn subagent_header_live_chrome_and_background_updates_reuse_large_bodies() {
    use ylva::DomainEvent;
    let mut state = ylva::AppState::new();
    state.enable_render_cache();
    let bubble_id = uuid::Uuid::new_v4();
    let run_id = uuid::Uuid::new_v4();
    let sections = vec![ylva::agent::tools::Section::Content(
        ylva::agent::tools::ContentSection {
            label: Some("Task".into()),
            content: "large task 日本語\n\n".repeat(3_000),
            file_extension: Some("md".into()),
            line_number_offset: None,
        },
    )];
    present_render_event(
        &mut state,
        DomainEvent::ToolStart {
            id: bubble_id,
            name: "spawn_subagent".into(),
            arguments: serde_json::json!({}),
            content_sections: sections,
            title_arguments: vec![("task".into(), serde_json::json!("Inspect the workspace"))],
            run_id: None,
        },
    );
    present_render_event(
        &mut state,
        DomainEvent::ChildAgentStarted {
            run_id,
            call_id: bubble_id,
            agent_type: "reviewer".into(),
            task: "Inspect".into(),
            label: "Inspect".into(),
        },
    );
    present_render_event(
        &mut state,
        DomainEvent::ChildAgentToken {
            run_id,
            text: "child transcript\n\n".repeat(3_000),
            is_thinking: false,
        },
    );
    present_render_event(
        &mut state,
        DomainEvent::ChildAgentDone {
            run_id,
            status: "success".into(),
            result: None,
        },
    );
    if matches!(
        state.chat_entries[0],
        ChatEntry::ToolBubble {
            collapsed: true,
            ..
        }
    ) {
        state.toggle_tool_bubble_collapsed(bubble_id);
    }
    state.switch_to_child_view(run_id);
    render_offline(&mut state, 120, 40);
    let header_body = Arc::clone(&state.render_cache.header.layouts[1]);
    let child_body = Arc::clone(&state.render_cache.conversation.layouts[1]);
    let builds = state.render_cache.body_layout_builds;
    assert!(state.render_cache.header.height > 3_000);
    assert_eq!(state.layout.header_bubble.height, 16);
    state.spinner_frame += 1;
    state.child_view.header_bubble_scroll_offset = 1_500;
    render_offline(&mut state, 120, 40);
    assert_eq!(builds, state.render_cache.body_layout_builds);
    assert!(Arc::ptr_eq(
        &header_body,
        &state.render_cache.header.layouts[1]
    ));
    assert!(Arc::ptr_eq(
        &child_body,
        &state.render_cache.conversation.layouts[1]
    ));
    present_render_event(
        &mut state,
        DomainEvent::ToolProgress {
            id: bubble_id,
            title_arguments: vec![(
                "details".into(),
                serde_json::json!("wrapping title ".repeat(30)),
            )],
        },
    );
    render_offline(&mut state, 120, 40);
    assert_eq!(builds, state.render_cache.body_layout_builds);
    assert!(Arc::ptr_eq(
        &header_body,
        &state.render_cache.header.layouts[1]
    ));
    assert!(state
        .child_view
        .header_rendered_lines
        .iter()
        .any(|line| line.contains("details=")));
    let second_run = uuid::Uuid::new_v4();
    present_render_event(
        &mut state,
        DomainEvent::ChildAgentStarted {
            run_id: second_run,
            call_id: uuid::Uuid::new_v4(),
            agent_type: "reviewer".into(),
            task: "other".into(),
            label: "other".into(),
        },
    );
    present_render_event(
        &mut state,
        DomainEvent::ChildAgentToken {
            run_id: second_run,
            text: "background text".into(),
            is_thinking: false,
        },
    );
    state.push_notice("root background notice".into());
    state.queue_steer_message("root background queued".into());
    state.steer_indicator = true;
    render_offline(&mut state, 120, 40);
    assert_eq!(builds, state.render_cache.body_layout_builds);
    assert!(state.render_cache.painted_rows <= 40);
    assert!(!state
        .conversation
        .chat_rendered_lines
        .iter()
        .any(|line| line.contains("background")));
    state.toggle_header_bubble_collapsed();
    render_offline(&mut state, 120, 40);
    state.toggle_header_bubble_collapsed();
    render_offline(&mut state, 120, 40);
    assert!(Arc::ptr_eq(
        &header_body,
        &state.render_cache.header.layouts[1]
    ));
    state.switch_to_root_view();
    assert!(state.render_cache.header.layouts.is_empty());
    assert!(state.render_cache.conversation.layouts.is_empty());
    assert!(state.child_view.header_rendered_lines.is_empty());
    render_offline(&mut state, 120, 40);
    assert!(state.render_cache.header.layouts.is_empty());
    assert!(state.pending_scroll_to_bubble.is_none());
}

/// While viewing an *active* subagent's own transcript, the footer must not
/// parrot the root's "Running tool…" word unless the child's own transcript
/// shows one of its own tool bubbles running — otherwise it misleadingly
/// implies the child itself is executing a tool when it's merely
/// streaming/thinking (regression test for the bug where the root's
/// `ToolRunning` mode — reported for the whole lifetime of a live child —
/// bled into the child view's footer verbatim).
#[test]
fn footer_says_subagent_working_while_child_is_only_streaming() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::{AgentMode, ChatEntry, ToolCallState};

    let backend = TestBackend::new(60, 20);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut state = ylva::TuiState {
        agent_mode: AgentMode::ToolRunning,
        child_view: ylva::tui_render_state::ChildViewState {
            child_view_bubble: Some((
                uuid::Uuid::new_v4(),
                "spawn_subagent".to_string(),
                ToolCallState::Running {
                    arguments: serde_json::json!({}),
                    content_sections: vec![],
                    title_arguments: vec![],
                    progress_arguments: vec![],
                },
                false,
            )),
            ..ylva::TuiState::new().child_view
        },
        chat_entries: vec![ChatEntry::AssistantMessage {
            text: "child is replying".to_string(),
            interrupted: false,
        }],
        ..ylva::TuiState::new()
    };
    let mut textarea = TextArea::default();

    terminal
        .draw(|frame| {
            tui_widgets::draw(
                frame,
                &mut state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            );
        })
        .expect("draw");

    let buf = terminal.backend().buffer().clone();
    let rendered: String = (0..20u16)
        .flat_map(|y| (0..60u16).map(move |x| (x, y)))
        .map(|(x, y)| buf.cell((x, y)).unwrap().symbol().to_string())
        .collect();
    assert!(
        rendered.contains("Subagent working"),
        "footer must say the subagent is working, not that a tool is running: {rendered:?}"
    );
    assert!(
        !rendered.contains("Running tool"),
        "the misleading inherited root mode word must not leak into the child view: {rendered:?}"
    );

    // Once the child's own transcript shows one of *its own* tool bubbles
    // running, "Running tool…" becomes accurate again and must be restored.
    state.chat_entries.push(ChatEntry::ToolBubble {
        id: uuid::Uuid::new_v4(),
        name: "read".to_string(),
        state: Box::new(ToolCallState::Running {
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
            progress_arguments: vec![],
        }),
        collapsed: false,
    });
    terminal
        .draw(|frame| {
            tui_widgets::draw(
                frame,
                &mut state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            );
        })
        .expect("draw with child running its own tool");
    let buf = terminal.backend().buffer().clone();
    let rendered: String = (0..20u16)
        .flat_map(|y| (0..60u16).map(move |x| (x, y)))
        .map(|(x, y)| buf.cell((x, y)).unwrap().symbol().to_string())
        .collect();
    assert!(
        rendered.contains("Running tool"),
        "once the child's own transcript shows a running tool bubble, the plain word is accurate again: {rendered:?}"
    );
}

#[test]
fn footer_shows_running_and_queued_subagent_counts() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::{ActiveSubagentCounts, TuiState};

    let backend = TestBackend::new(80, 20);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut state = TuiState {
        active_subagent_counts: ActiveSubagentCounts {
            running: 2,
            queued: 1,
        },
        ..TuiState::new()
    };
    let mut textarea = TextArea::default();

    terminal
        .draw(|frame| {
            tui_widgets::draw(
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
    let rendered: String = (0..20u16)
        .flat_map(|row| (0..80u16).map(move |column| (column, row)))
        .map(|(column, row)| buffer.cell((column, row)).unwrap().symbol().to_string())
        .collect();
    assert!(
        rendered.contains(" · Subagents: 2 running, 1 queued"),
        "subagent counts should follow the busy indicator with a middle-dot separator: {rendered:?}"
    );
}

/// While viewing a child that is streaming extended-thinking/reasoning text
/// (no visible reply yet), the footer must say "Subagent thinking…" rather
/// than the generic "Subagent working…" — mirrors the root's own
/// "Thinking…"/"Streaming…" distinction, now that the child's own
/// `partial_thinking_text` is projected separately from its
/// `partial_assistant_text` (see `render::project_for_draw`).
#[test]
fn footer_says_subagent_thinking_while_child_streams_thinking_text() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::{AgentMode, ToolCallState};

    let backend = TestBackend::new(60, 20);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut state = ylva::TuiState {
        agent_mode: AgentMode::ToolRunning,
        child_view: ylva::tui_render_state::ChildViewState {
            child_view_bubble: Some((
                uuid::Uuid::new_v4(),
                "spawn_subagent".to_string(),
                ToolCallState::Running {
                    arguments: serde_json::json!({}),
                    content_sections: vec![],
                    title_arguments: vec![],
                    progress_arguments: vec![],
                },
                false,
            )),
            ..ylva::TuiState::new().child_view
        },
        partial_thinking_text: "pondering the flaky test...".to_string(),
        ..ylva::TuiState::new()
    };
    let mut textarea = TextArea::default();

    terminal
        .draw(|frame| {
            tui_widgets::draw(
                frame,
                &mut state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            );
        })
        .expect("draw");

    let buf = terminal.backend().buffer().clone();
    let rendered: String = (0..20u16)
        .flat_map(|y| (0..60u16).map(move |x| (x, y)))
        .map(|(x, y)| buf.cell((x, y)).unwrap().symbol().to_string())
        .collect();
    assert!(
        rendered.contains("Subagent thinking"),
        "footer must say the subagent is thinking, not generically working: {rendered:?}"
    );
}

/// While viewing a child that is streaming its visible reply text, the
/// footer must say "Subagent streaming…" rather than the generic "Subagent
/// working…" — mirrors the root's own wording for the same case.
#[test]
fn footer_says_subagent_streaming_while_child_streams_response_text() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::{AgentMode, ToolCallState};

    let backend = TestBackend::new(60, 20);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut state = ylva::TuiState {
        agent_mode: AgentMode::ToolRunning,
        child_view: ylva::tui_render_state::ChildViewState {
            child_view_bubble: Some((
                uuid::Uuid::new_v4(),
                "spawn_subagent".to_string(),
                ToolCallState::Running {
                    arguments: serde_json::json!({}),
                    content_sections: vec![],
                    title_arguments: vec![],
                    progress_arguments: vec![],
                },
                false,
            )),
            ..ylva::TuiState::new().child_view
        },
        partial_assistant_text: "here is what I found so far".to_string(),
        ..ylva::TuiState::new()
    };
    let mut textarea = TextArea::default();

    terminal
        .draw(|frame| {
            tui_widgets::draw(
                frame,
                &mut state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            );
        })
        .expect("draw");

    let buf = terminal.backend().buffer().clone();
    let rendered: String = (0..20u16)
        .flat_map(|y| (0..60u16).map(move |x| (x, y)))
        .map(|(x, y)| buf.cell((x, y)).unwrap().symbol().to_string())
        .collect();
    assert!(
        rendered.contains("Subagent streaming"),
        "footer must say the subagent is streaming, not generically working: {rendered:?}"
    );
}

#[test]
fn spawn_subagent_running_duration_is_visible_in_parent_and_child_views() {
    use ratatui_textarea::TextArea;
    use ylva::tui_reducer_state::ResultSection;
    use ylva::{AppState, DomainEvent, TuiEvent};

    for (child_view, cached) in [(false, false), (true, false), (false, true), (true, true)] {
        for collapsed in [false, true] {
            let bubble_id = uuid::Uuid::new_v4();
            let run_id = uuid::Uuid::new_v4();
            let mut state = AppState::new();
            if cached {
                state.enable_render_cache();
            }
            let apply_event = |state: &mut AppState, event| {
                ylva::presenter::update(state, TuiEvent::Domain(event), &empty_textarea_context());
            };
            apply_event(
                &mut state,
                DomainEvent::ToolStart {
                    id: bubble_id,
                    name: "spawn_subagent".to_string(),
                    arguments: serde_json::json!({"label": "Inspect"}),
                    content_sections: vec![],
                    title_arguments: vec![("label".to_string(), serde_json::json!("Inspect"))],
                    run_id: None,
                },
            );
            if let ChatEntry::ToolBubble {
                collapsed: bubble_collapsed,
                ..
            } = &mut state.chat_entries[0]
            {
                *bubble_collapsed = collapsed;
            }
            let mut terminal = Terminal::new(TestBackend::new(80, 20)).expect("terminal");
            let mut textarea = TextArea::default();
            let mut draw_title = |state: &mut AppState| {
                terminal
                    .draw(|frame| {
                        ylva::render::render(
                            frame,
                            state,
                            &mut textarea,
                            None,
                            None,
                            &ToolRegistry::new(),
                        );
                    })
                    .expect("draw");
                let lines = if child_view {
                    &state.child_view.header_rendered_lines
                } else {
                    &state.conversation.chat_rendered_lines
                };
                lines
                    .iter()
                    .find(|line| line.contains("Subagent"))
                    .expect("subagent title")
                    .clone()
            };
            if !child_view {
                let title = draw_title(&mut state);
                assert!(!title.ends_with('s'), "admission is not execution: {title}");
            }
            apply_event(
                &mut state,
                DomainEvent::ChildAgentStarted {
                    run_id,
                    call_id: bubble_id,
                    agent_type: "default".to_string(),
                    task: "Inspect".to_string(),
                    label: "Inspect".to_string(),
                },
            );
            if child_view {
                state.switch_to_child_view(run_id);
            }
            for elapsed_seconds in [12, 22] {
                state
                    .child_conversations
                    .get_mut(&run_id)
                    .expect("child")
                    .started_at = Some(
                    std::time::Instant::now() - Duration::from_millis(elapsed_seconds * 1000 + 100),
                );
                let started_at = state.child_conversations[&run_id].started_at;
                apply_event(
                    &mut state,
                    DomainEvent::ToolProgress {
                        id: bubble_id,
                        title_arguments: vec![("status".to_string(), serde_json::json!("running"))],
                    },
                );
                assert_eq!(state.child_conversations[&run_id].started_at, started_at);
                let title = draw_title(&mut state);
                let duration_label = title.split_whitespace().last().expect("duration");
                assert_eq!(
                    duration_label,
                    format!("{elapsed_seconds}s"),
                    "running duration uses whole seconds in the {} view",
                    if child_view { "child" } else { "parent" },
                );
                let previous_layouts = if child_view {
                    state.render_cache.header.layouts.clone()
                } else {
                    state.render_cache.conversation.layouts.clone()
                };
                state
                    .child_conversations
                    .get_mut(&run_id)
                    .expect("child")
                    .started_at = Some(
                    std::time::Instant::now() - Duration::from_millis(elapsed_seconds * 1000 + 600),
                );
                let same_second_title = draw_title(&mut state);
                assert_eq!(
                    same_second_title, title,
                    "subsecond redraws must keep the timer calm"
                );
                if cached {
                    let current_layouts = if child_view {
                        &state.render_cache.header.layouts
                    } else {
                        &state.render_cache.conversation.layouts
                    };
                    assert_eq!(previous_layouts.len(), current_layouts.len());
                    assert!(
                        previous_layouts
                            .iter()
                            .zip(current_layouts)
                            .all(|(previous, current)| Arc::ptr_eq(previous, current)),
                        "unchanged elapsed seconds and spinner must reuse cached chrome and bodies"
                    );
                }
            }
            apply_event(
                &mut state,
                DomainEvent::ToolProgress {
                    id: bubble_id,
                    title_arguments: vec![("status".to_string(), serde_json::json!("queued"))],
                },
            );
            let title = draw_title(&mut state);
            assert!(
                !title.ends_with('s'),
                "queued calls must not show a running duration: {title}"
            );
            apply_event(
                &mut state,
                DomainEvent::ToolDone {
                    id: bubble_id,
                    name: "spawn_subagent".to_string(),
                    result_section: ResultSection::plain("Finished"),
                    summary: vec![],
                    duration: Duration::from_millis(1234),
                    content_sections: vec![],
                    run_id: None,
                },
            );
            let title = draw_title(&mut state);
            assert!(
                title.ends_with("1.234s"),
                "completion must replace the live timer: {title}"
            );
        }
    }
}

/// The drill-in header shortens the root-relative "Spawn Subagent" title.
#[test]
fn child_view_header_shortens_spawn_subagent_title() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::ToolCallState;

    let backend = TestBackend::new(60, 20);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut state = ylva::TuiState {
        child_view: ylva::tui_render_state::ChildViewState {
            child_view_bubble: Some((
                uuid::Uuid::new_v4(),
                "spawn_subagent".to_string(),
                ToolCallState::Running {
                    arguments: serde_json::json!({}),
                    content_sections: vec![],
                    title_arguments: vec![],
                    progress_arguments: vec![],
                },
                false,
            )),
            ..ylva::TuiState::new().child_view
        },
        ..ylva::TuiState::new()
    };
    let mut textarea = TextArea::default();

    terminal
        .draw(|frame| {
            tui_widgets::draw(
                frame,
                &mut state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            );
        })
        .expect("draw");

    let buf = terminal.backend().buffer().clone();
    let rendered: String = (0..20u16)
        .flat_map(|y| (0..60u16).map(move |x| (x, y)))
        .map(|(x, y)| buf.cell((x, y)).unwrap().symbol().to_string())
        .collect();
    assert!(
        rendered.contains("Subagent"),
        "header must show the shortened title: {rendered:?}"
    );
    assert!(
        !rendered.contains("Spawn Subagent"),
        "the plain title-cased tool name must not appear in the header: {rendered:?}"
    );
}

/// While viewing a finished subagent's transcript, if the root loop is still
/// busy (e.g. producing its final reply from the child's result), the
/// footer must say so explicitly instead of the plain "Streaming…"/"Running
/// tool…" word, which would misleadingly read as if the *subagent* — not
/// the parent — were still working (regression test for the ambiguous
/// footer text bug).
#[test]
fn footer_indicates_subagent_done_while_parent_still_busy() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::{AgentMode, ToolCallState};

    let backend = TestBackend::new(60, 20);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut state = ylva::TuiState {
        parent_agent_mode: AgentMode::Streaming,
        agent_mode: AgentMode::Streaming,
        child_view: ylva::tui_render_state::ChildViewState {
            child_view_bubble: Some((
                uuid::Uuid::new_v4(),
                "spawn_subagent".to_string(),
                ToolCallState::Killed,
                false,
            )),
            ..ylva::TuiState::new().child_view
        },
        ..ylva::TuiState::new()
    };
    let mut textarea = TextArea::default();

    terminal
        .draw(|frame| {
            tui_widgets::draw(
                frame,
                &mut state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            );
        })
        .expect("draw");

    let buf = terminal.backend().buffer().clone();
    let rendered: String = (0..20u16)
        .flat_map(|y| (0..60u16).map(move |x| (x, y)))
        .map(|(x, y)| buf.cell((x, y)).unwrap().symbol().to_string())
        .collect();
    assert!(
        rendered.contains("Subagent done"),
        "footer must call out that the subagent is done while the parent is still busy: {rendered:?}"
    );
    assert!(
        !rendered.contains("Streaming…"),
        "the ambiguous plain mode word must not also be shown: {rendered:?}"
    );

    // Once the root also goes idle, the plain "Idle" word is unambiguous —
    // no special-casing needed (nothing is left "working" to disambiguate).
    state.parent_agent_mode = AgentMode::Idle;
    state.agent_mode = AgentMode::Idle;
    terminal
        .draw(|frame| {
            tui_widgets::draw(
                frame,
                &mut state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            );
        })
        .expect("draw after parent idle");
    let buf = terminal.backend().buffer().clone();
    let rendered: String = (0..20u16)
        .flat_map(|y| (0..60u16).map(move |x| (x, y)))
        .map(|(x, y)| buf.cell((x, y)).unwrap().symbol().to_string())
        .collect();
    assert!(
        rendered.contains("Idle"),
        "once the parent is also idle, the plain mode word must show: {rendered:?}"
    );
    assert!(
        !rendered.contains("Subagent done"),
        "the override must not linger once both sides are idle: {rendered:?}"
    );
}

#[test]
fn subagent_bubble_title_carries_the_agent_type_as_a_subtype() {
    use ylva::agent::tools::spawn_subagent::SpawnSubagentTool;
    use ylva::tui_reducer_state::ResultSection;
    use ylva::ToolCallState;

    let mut registry = ylva::ToolRegistry::new();
    registry.register(Arc::new(SpawnSubagentTool::new(false)));

    let mut state = ylva::TuiState {
        chat_entries: vec![ChatEntry::ToolBubble {
            id: uuid::Uuid::new_v4(),
            name: SpawnSubagentTool::NAME.to_string(),
            state: Box::new(ToolCallState::Done {
                arguments: serde_json::json!({
                    "label": "Analyze read tool features",
                    "task": "list the read tool's features",
                    "agent_type": "local-discovery",
                }),
                result_section: ResultSection::plain(""),
                content_sections: vec![],
                summary: vec![("status".to_string(), "unstructured".to_string())],
                duration: std::time::Duration::from_millis(48_392),
                title_arguments: vec![
                    (
                        "label".to_string(),
                        serde_json::json!("Analyze read tool features"),
                    ),
                    (
                        "agent_type".to_string(),
                        serde_json::json!("local-discovery"),
                    ),
                ],
            }),
            collapsed: true,
        }],
        ..ylva::TuiState::new()
    };

    let rows = render_tool_bubble_rows_with_registry(&mut state, 120, 20, &registry);
    let title_row = rows
        .iter()
        .find(|row| row.contains("Spawn Subagent"))
        .expect("the collapsed subagent title row renders");

    assert!(
        title_row.contains("Spawn Subagent (local-discovery)"),
        "the subtype reads as part of the title: {title_row:?}"
    );
    assert!(
        !title_row.contains("agent_type"),
        "the subtype must not be rendered a second time as a prop: {title_row:?}"
    );
    assert!(
        title_row.contains("Analyze read tool features"),
        "the label stays the inline primary: {title_row:?}"
    );
    assert!(
        title_row.contains("status=unstructured"),
        "the summary is untouched by the subtype promotion: {title_row:?}"
    );
}
