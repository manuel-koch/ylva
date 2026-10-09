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
fn live_thinking_timers_use_whole_seconds_and_preserve_completed_precision() {
    use ylva::DomainEvent;

    for cached in [false, true] {
        for child_view in [false, true] {
            for collapsed in [false, true] {
                let mut state = ylva::AppState::new();
                if cached {
                    state.enable_render_cache();
                }
                let run_id = uuid::Uuid::new_v4();
                if child_view {
                    let bubble_id = uuid::Uuid::new_v4();
                    present_render_event(
                        &mut state,
                        DomainEvent::ToolStart {
                            id: bubble_id,
                            name: "spawn_subagent".into(),
                            arguments: serde_json::json!({}),
                            content_sections: vec![],
                            title_arguments: vec![],
                            run_id: None,
                        },
                    );
                    present_render_event(
                        &mut state,
                        DomainEvent::ChildAgentStarted {
                            run_id,
                            call_id: bubble_id,
                            agent_type: "default".into(),
                            task: "Inspect".into(),
                            label: "Inspect".into(),
                        },
                    );
                    present_render_event(
                        &mut state,
                        DomainEvent::ChildAgentToken {
                            run_id,
                            text: "reasoning".into(),
                            is_thinking: true,
                        },
                    );
                    state.switch_to_child_view(run_id);
                } else {
                    present_render_event(&mut state, DomainEvent::StreamStarted);
                    present_render_event(
                        &mut state,
                        DomainEvent::StreamThinkingChunk("reasoning".into()),
                    );
                }
                for elapsed_millis in [9_100, 9_600, 10_100] {
                    let turn_buffer = if child_view {
                        &mut state
                            .child_conversations
                            .get_mut(&run_id)
                            .expect("child")
                            .turn_buffer
                    } else {
                        &mut state.turn_buffer
                    };
                    turn_buffer.partial_thinking_collapsed = collapsed;
                    turn_buffer.thinking_started_at =
                        Some(std::time::Instant::now() - Duration::from_millis(elapsed_millis));
                    render_offline(&mut state, 120, 40);
                    let title = state
                        .conversation
                        .chat_rendered_lines
                        .iter()
                        .find(|line| line.contains("Thinking… ("))
                        .expect("thinking title");
                    assert!(
                        title.ends_with(&format!("Thinking… ({}s)", elapsed_millis / 1000)),
                        "live thinking duration must truncate to whole seconds: {title}"
                    );
                }
                let completed = ChatEntry::ThinkingMessage {
                    text: "reasoning".into(),
                    collapsed,
                    duration: Duration::from_millis(1234),
                };
                if child_view {
                    let child = state.child_conversations.get_mut(&run_id).expect("child");
                    child.turn_buffer = Default::default();
                    child.messages.push(completed);
                } else {
                    state.turn_buffer = Default::default();
                    state.chat_entries.push(completed);
                }
                render_offline(&mut state, 120, 40);
                assert!(state
                    .conversation
                    .chat_rendered_lines
                    .iter()
                    .any(|line| line.ends_with("Thought for 1.234s")));
            }
        }
    }
}

#[tokio::test]
async fn typed_enter_creates_sent_user_bubble_and_assistant() {
    let provider = MockProvider::new(vec!["hello".to_string()], Duration::from_millis(5));
    let (backend, last_buffer, _) = InspectableTestBackend::new(80, 24);
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

    type_and_enter(&action_tx, "hi").await;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let rendered = rendered_text(&last_buffer.lock().expect("buffer snapshot lock"));
            if rendered.contains("hello") && !rendered.contains("Streaming") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("completed assistant frame appears before closing the action channel");
    drop(action_tx);

    let state = run_handle.await.expect("task joined");

    assert_eq!(
        state.chat_entries[0],
        ChatEntry::UserMessage {
            text: "hi".to_string(),
            state: UserMessageState::Sent
        }
    );
    assert!(
        state.chat_entries.iter().any(is_assistant_message),
        "assistant bubble should be committed: {:?}",
        state.chat_entries
    );
}

#[tokio::test]
async fn double_press_ctrl_c_mid_stream_cancels_pending_steer() {
    let run = GatedTuiRun::new(None);
    type_and_enter(&run.actions, "first").await;
    run.start_response(1).await;
    type_and_enter(&run.actions, "steer").await;
    run.wait_for_text("1 message queued").await;
    send(&run.actions, AppAction::CtrlC).await;
    run.wait_for_text("Press ctrl-c again").await;
    send(&run.actions, AppAction::CtrlC).await;
    let state = run.wait_for_exit().await;

    assert_eq!(state.agent_mode, ylva::AgentMode::Idle);
    let steer = state
        .chat_entries
        .iter()
        .find(|entry| matches!(entry, ChatEntry::UserMessage { text, .. } if text == "steer"))
        .expect("steer bubble present");
    assert_eq!(
        steer,
        &ChatEntry::UserMessage {
            text: "steer".to_string(),
            state: UserMessageState::Cancelled
        }
    );
}

#[tokio::test]
async fn ctrl_c_then_mismatched_escape_does_not_stop_stream() {
    // Regression: Ctrl-C arms its own quit-confirm window. A *different* key
    // (Esc) must not confirm it — it starts Esc's own fresh confirm window
    // instead, leaving the current turn (and any queued steer) to complete
    // normally.
    let run = GatedTuiRun::new(None);
    type_and_enter(&run.actions, "first").await;
    run.start_response(1).await;
    type_and_enter(&run.actions, "steer").await;
    run.wait_for_text("1 message queued").await;
    send(&run.actions, AppAction::CtrlC).await;
    run.wait_for_text("Press ctrl-c again").await;
    send(&run.actions, AppAction::Escape).await;
    run.wait_for_text("Press esc again").await;
    run.provider.release_response.add_permits(1);
    run.start_response(2).await;
    run.finish_response(2).await;
    let state = run.close().await;
    assert_eq!(state.agent_mode, ylva::AgentMode::Idle);
    assert!(
        !state.chat_entries.iter().any(|entry| matches!(
            entry,
            ChatEntry::UserMessage {
                state: UserMessageState::Cancelled,
                ..
            }
        )),
        "queued steer must not be cancelled by a mismatched-key double press: {:?}",
        state.chat_entries
    );
}

#[tokio::test]
async fn steer_cycle_leaves_no_queued_entries() {
    let run = GatedTuiRun::new(None);
    type_and_enter(&run.actions, "first").await;
    run.start_response(1).await;
    type_and_enter(&run.actions, "steer").await;
    run.wait_for_text("1 message queued").await;
    run.provider.release_response.add_permits(1);
    run.start_response(2).await;
    run.finish_response(2).await;
    let state = run.close().await;
    assert!(
        !state.chat_entries.iter().any(|entry| matches!(
            entry,
            ChatEntry::UserMessage {
                state: UserMessageState::Queued,
                ..
            }
        )),
        "no Queued entries should remain after the steer cycle: {:?}",
        state.chat_entries
    );
    assert!(!state.steer_indicator);
}

#[tokio::test]
async fn initial_prompt_produces_sent_bubble() {
    let run = GatedTuiRun::new(Some("init".into()));
    run.start_response(1).await;
    run.finish_response(1).await;
    let state = run.close().await;
    assert_eq!(
        state.chat_entries[0],
        ChatEntry::UserMessage {
            text: "init".to_string(),
            state: UserMessageState::Sent
        }
    );
}

#[test]
fn steer_bubble_renders_below_streaming_not_above() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::AgentMode;

    let backend = TestBackend::new(60, 20);
    let mut terminal = Terminal::new(backend).expect("terminal");

    // Equivalent to: push_user_bubble("hello", Sent) → StreamStarted →
    // AppendText("world") → queue_steer_message("steer me"). Built directly
    // (rather than through a reducer) since this test targets
    // `tui_widgets::draw_chat`'s deferred-rendering order, not state transitions.
    // Queued steers are staged in `queued_steer_texts`, not `chat_entries`
    // (see `AppState::queue_steer_message`).
    let mut state = ylva::TuiState {
        chat_entries: vec![ChatEntry::UserMessage {
            text: "hello".to_string(),
            state: UserMessageState::Sent,
        }],
        queued_steer_texts: vec!["steer me".to_string()],
        partial_assistant_text: "world".to_string(),
        agent_mode: AgentMode::Streaming,
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
    // Collect non-empty rows as strings.
    let rows: Vec<String> = (0..20u16)
        .map(|y| {
            (0..60u16)
                .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                .collect::<String>()
        })
        .collect();

    // Find the row that contains "world" (streaming bubble) and "steer me".
    let stream_row = rows
        .iter()
        .position(|r| r.contains("world"))
        .expect("stream text visible");
    let steer_row = rows
        .iter()
        .position(|r| r.contains("steer me"))
        .expect("steer text visible");

    assert!(
        steer_row > stream_row,
        "steer bubble (row {steer_row}) should be BELOW streaming text (row {stream_row})"
    );
}

#[test]
fn steer_sent_before_first_chunk_does_not_jump_above_stream() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::AgentMode;

    let backend = TestBackend::new(60, 20);
    let mut terminal = Terminal::new(backend).expect("terminal");

    // Stream started but no text yet; user already queued a steer.
    let mut state = ylva::TuiState {
        chat_entries: vec![ChatEntry::UserMessage {
            text: "hello".to_string(),
            state: UserMessageState::Sent,
        }],
        queued_steer_texts: vec!["steer me".to_string()],
        agent_mode: AgentMode::Streaming,
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

    // "hello" must appear before "steer me"; no streaming bubble yet (no text).
    let buf = terminal.backend().buffer().clone();
    let rows: Vec<String> = (0..20u16)
        .map(|y| {
            (0..60u16)
                .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                .collect()
        })
        .collect();

    let hello_row = rows
        .iter()
        .position(|r| r.contains("hello"))
        .expect("hello visible");
    let steer_row = rows
        .iter()
        .position(|r| r.contains("steer me"))
        .expect("steer visible");
    assert!(
        steer_row > hello_row,
        "steer (row {steer_row}) should be below hello (row {hello_row})"
    );

    // Now simulate the first chunk arriving; steer must still be below it.
    state.partial_assistant_text.push_str("world");
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
        .expect("draw after chunk");

    let buf = terminal.backend().buffer().clone();
    let rows: Vec<String> = (0..20u16)
        .map(|y| {
            (0..60u16)
                .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                .collect()
        })
        .collect();

    let stream_row = rows
        .iter()
        .position(|r| r.contains("world"))
        .expect("stream text visible");
    let steer_row = rows
        .iter()
        .position(|r| r.contains("steer me"))
        .expect("steer still visible");
    assert!(
        steer_row > stream_row,
        "steer (row {steer_row}) must be below streaming text (row {stream_row})"
    );
}

#[test]
fn footer_shows_the_active_named_engine_and_model() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::{EngineDisplayInfo, TuiState};

    let backend = TestBackend::new(60, 20);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut state = TuiState {
        engine_display: EngineDisplayInfo {
            engine_name: "ollama/local".to_string(),
            provider_connection: "ollama".to_string(),
            model: "llama3.2".to_string(),
            reasoning_effort: Some("max".to_string()),
            compaction_engine: None,
            inherits_parent_runtime: false,
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
        .flat_map(|row| (0..60u16).map(move |column| (column, row)))
        .map(|(column, row)| buffer.cell((column, row)).unwrap().symbol().to_string())
        .collect();
    assert!(
        rendered.contains("ollama/local → llama3.2 (max)"),
        "footer must show the active named-engine/model identity: {rendered:?}"
    );
    assert!(
        !rendered.contains(" → ollama → "),
        "footer must omit the provider connection identity: {rendered:?}"
    );

    state.engine_display.reasoning_effort = None;
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
        .expect("draw without explicit reasoning effort");

    let buffer = terminal.backend().buffer().clone();
    let rendered_without_reasoning: String = (0..20u16)
        .flat_map(|row| (0..60u16).map(move |column| (column, row)))
        .map(|(column, row)| buffer.cell((column, row)).unwrap().symbol().to_string())
        .collect();
    assert!(
        rendered_without_reasoning.contains("ollama/local → llama3.2"),
        "footer must retain the active named-engine/model identity: {rendered_without_reasoning:?}"
    );
    assert!(
        !rendered_without_reasoning.contains("ollama/local → llama3.2 ("),
        "footer must omit the default reasoning effort: {rendered_without_reasoning:?}"
    );
}

#[test]
fn compaction_marker_renders_between_the_summarized_and_the_verbatim_turns() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::{ChatEntry, TuiState, UserMessageState};

    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
    let mut state = TuiState {
        chat_entries: vec![
            ChatEntry::UserMessage {
                text: "the old question".to_string(),
                state: UserMessageState::Sent,
            },
            ChatEntry::AssistantMessage {
                text: "the old answer".to_string(),
                interrupted: false,
            },
            ChatEntry::CompactionMarker {
                summarized_messages: 2,
            },
            ChatEntry::UserMessage {
                text: "the tail question".to_string(),
                state: UserMessageState::Sent,
            },
        ],
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
    let rows: Vec<String> = (0..24u16)
        .map(|row| {
            (0..80u16)
                .map(|column| buffer.cell((column, row)).unwrap().symbol().to_string())
                .collect()
        })
        .collect();
    let rendered = rows.join("\n");
    assert!(
        rendered.contains("compaction · 2 messages summarized"),
        "the marker is rendered: {rendered:?}"
    );
    assert!(
        rendered.contains("the old question") && rendered.contains("the old answer"),
        "the summarized prefix stays browsable: {rendered:?}"
    );
    assert!(
        rendered.contains("the tail question"),
        "the verbatim tail stays browsable: {rendered:?}"
    );
    let marker_row = rows
        .iter()
        .position(|row| row.contains("compaction · 2 messages summarized"))
        .expect("marker row");
    let tail_row = rows
        .iter()
        .position(|row| row.contains("the tail question"))
        .expect("tail row");
    assert!(
        marker_row < tail_row,
        "the marker precedes the tail it opens: marker={marker_row} tail={tail_row}"
    );
}

#[test]
fn footer_shows_compacting_independently_of_agent_mode() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::{tui_widgets, AgentMode, TuiState};

    for agent_mode in [AgentMode::Idle, AgentMode::Streaming] {
        for compacting in [false, true] {
            let mut terminal = Terminal::new(TestBackend::new(100, 20)).expect("terminal");
            let mut state = TuiState {
                agent_mode,
                compacting,
                ..TuiState::new()
            };
            terminal
                .draw(|frame| {
                    tui_widgets::draw(
                        frame,
                        &mut state,
                        &mut TextArea::default(),
                        None,
                        None,
                        &ylva::ToolRegistry::new(),
                    );
                })
                .expect("draw");
            let buffer = terminal.backend().buffer();
            let rendered: String = (0..20u16)
                .flat_map(|row| (0..100u16).map(move |column| (column, row)))
                .map(|position| buffer.cell(position).unwrap().symbol())
                .collect();
            assert_eq!(rendered.contains("Compacting…"), compacting);
            assert_eq!(state.agent_mode, agent_mode);
        }
    }
}

#[test]
fn approval_footer_keeps_context_usage_beside_buttons() {
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::TuiState;

    for is_subagent in [false, true] {
        for denial_reason in ["", "Do not execute this"] {
            let mut terminal = Terminal::new(TestBackend::new(80, 20)).expect("terminal");
            let mut state = TuiState {
                context_usage_text: Some("64.2K (45%)".to_string()),
                pending_approval: Some((
                    uuid::Uuid::new_v4(),
                    "bash".to_string(),
                    serde_json::json!({"command": "echo hello"}),
                )),
                pending_approval_is_subagent: is_subagent,
                ..TuiState::new()
            };
            let mut textarea = TextArea::default();
            textarea.insert_str(denial_reason);
            let mut buttons = None;
            terminal
                .draw(|frame| {
                    buttons = tui_widgets::draw(
                        frame,
                        &mut state,
                        &mut textarea,
                        None,
                        None,
                        &ylva::ToolRegistry::new(),
                    );
                })
                .expect("draw");
            let buttons = buttons.expect("approval buttons");
            let buffer = terminal.backend().buffer();
            let line: String = (0..80)
                .map(|column| buffer.cell((column, buttons.approve.y)).unwrap().symbol())
                .collect();
            assert!(
                line.ends_with("64.2K (45%)"),
                "approval row must retain the right-aligned readout: {line:?}"
            );
            assert!(line.contains("Approve") && line.contains("Deny"));
            assert!(buttons.deny.right() <= 80 - "64.2K (45%)".len() as u16);
        }
    }
}

#[test]
fn footer_shows_the_model_view_readout_right_aligned_on_the_hint_row() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::TuiState;

    let mut terminal = Terminal::new(TestBackend::new(80, 20)).expect("terminal");
    let mut state = TuiState {
        context_usage_text: Some("64.2K (45%)".to_string()),
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
    let hint_row = (0..20u16)
        .find(|row| {
            let line: String = (0..80u16)
                .map(|column| buffer.cell((column, *row)).unwrap().symbol().to_string())
                .collect();
            line.contains("F1 help")
        })
        .expect("keybinding footer row");
    let line: String = (0..80u16)
        .map(|column| {
            buffer
                .cell((column, hint_row))
                .unwrap()
                .symbol()
                .to_string()
        })
        .collect();

    assert!(
        line.contains("64.2K (45%)"),
        "the hint row carries the context readout: {line:?}"
    );
    assert!(
        line.starts_with(' ') && line.contains("enter send"),
        "the keybinding hints stay left-aligned: {line:?}"
    );
    let readout_end = line
        .char_indices()
        .rev()
        .find(|(_, character)| !character.is_whitespace())
        .map(|(index, _)| index)
        .expect("non-blank readout");
    assert_eq!(
        readout_end,
        line.trim_end().len() - 1,
        "the readout sits at the right end of the row: {line:?}"
    );
    assert!(
        !line.contains("enter send   64.2K"),
        "hints and readout must not run together: {line:?}"
    );
}

#[test]
fn narrow_approval_footer_preserves_buttons_without_context_usage() {
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::TuiState;

    let mut terminal = Terminal::new(TestBackend::new(50, 20)).expect("terminal");
    let mut state = TuiState {
        context_usage_text: Some("64.2K (45%)".to_string()),
        pending_approval: Some((
            uuid::Uuid::new_v4(),
            "bash".to_string(),
            serde_json::json!({"command": "echo hello"}),
        )),
        ..TuiState::new()
    };
    let mut textarea = TextArea::default();
    terminal
        .draw(|frame| {
            let buttons = tui_widgets::draw(
                frame,
                &mut state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            )
            .expect("approval buttons");
            assert!(buttons.deny.right() <= 50);
        })
        .expect("draw");
    let buffer = terminal.backend().buffer();
    let rendered: String = (0..20)
        .flat_map(|row| (0..50).map(move |column| buffer.cell((column, row)).unwrap().symbol()))
        .collect();
    assert!(!rendered.contains("64.2K (45%)"), "{rendered:?}");
    assert!(rendered.contains("Approve (enter)") && rendered.contains("Deny (esc)"));
}

#[test]
fn footer_renders_bold_keys_with_normal_action_text_and_separators() {
    use ratatui::backend::TestBackend;
    use ratatui::style::Modifier;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::{AgentMode, TuiState};

    let mut terminal = Terminal::new(TestBackend::new(80, 20)).expect("terminal");
    let mut state = TuiState {
        agent_mode: AgentMode::Streaming,
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
    let footer_row = (0..20u16)
        .find(|row| {
            let line: String = (0..80u16)
                .map(|column| buffer.cell((column, *row)).unwrap().symbol().to_string())
                .collect();
            line.contains("enter send · esc cancel · ctrl-c cancel & quit · F1 help")
        })
        .expect("keybinding footer row");
    let line: String = (0..80u16)
        .map(|column| {
            buffer
                .cell((column, footer_row))
                .unwrap()
                .symbol()
                .to_string()
        })
        .collect();
    let enter_start = line.find("enter").expect("enter keybinding") as u16;
    let send_start = enter_start + "enter ".chars().count() as u16;
    let separator_start = line.find(" · ").expect("separator") as u16;

    for column in enter_start..enter_start + "enter".chars().count() as u16 {
        assert!(
            buffer
                .cell((column, footer_row))
                .expect("enter cell")
                .style()
                .add_modifier
                .contains(Modifier::BOLD),
            "key cell at column {column} should be bold"
        );
    }
    for column in send_start..send_start + "send".chars().count() as u16 {
        assert!(
            !buffer
                .cell((column, footer_row))
                .expect("send cell")
                .style()
                .add_modifier
                .contains(Modifier::BOLD),
            "action cell at column {column} should not be bold"
        );
    }
    for column in separator_start..separator_start + " · ".chars().count() as u16 {
        assert!(
            !buffer
                .cell((column, footer_row))
                .expect("separator cell")
                .style()
                .add_modifier
                .contains(Modifier::BOLD),
            "separator cell at column {column} should not be bold"
        );
    }
}

/// While viewing the *root* transcript, a still-running `spawn_subagent`
/// tool bubble makes `agent_mode()` report `ToolRunning` for the whole
/// lifetime of the live child (see `AppState::agent_mode`) — even while the
/// root loop itself is mid-stream on its own next iteration (e.g. deciding
/// what to do once the child finishes). Before this fix, the in-flight
/// thinking/response bubble was gated on `agent_mode == AgentMode::Streaming`
/// and stayed completely invisible in that window, only appearing once
/// flushed into a fully-committed bubble — i.e. it "popped in" with no
/// visible streaming progress (regression test for
/// docs/known-test-issues.md: "After main agent has spawned multiple subagents
/// ... a thinking bubble pops up in the conversation without any previous
/// thinking-progress").
#[test]
fn root_view_shows_live_thinking_progress_while_a_tool_bubble_is_running() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::{AgentMode, ChatEntry, ToolCallState};

    let backend = TestBackend::new(60, 20);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut state = ylva::TuiState {
        agent_mode: AgentMode::ToolRunning,
        chat_entries: vec![ChatEntry::ToolBubble {
            id: uuid::Uuid::new_v4(),
            name: "spawn_subagent".to_string(),
            state: Box::new(ToolCallState::Running {
                arguments: serde_json::json!({}),
                content_sections: vec![],
                title_arguments: vec![],
                progress_arguments: vec![],
            }),
            collapsed: false,
        }],
        partial_thinking_text: "pondering while the subagent runs...".to_string(),
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
        rendered.contains("pondering while the subagent runs"),
        "in-flight thinking text must render live, not only once flushed: {rendered:?}"
    );

    // Same for in-flight assistant reply text.
    state.partial_thinking_text.clear();
    state.partial_assistant_text = "here is my plan while waiting...".to_string();
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
        .expect("draw with in-flight response text");
    let buf = terminal.backend().buffer().clone();
    let rendered: String = (0..20u16)
        .flat_map(|y| (0..60u16).map(move |x| (x, y)))
        .map(|(x, y)| buf.cell((x, y)).unwrap().symbol().to_string())
        .collect();
    assert!(
        rendered.contains("here is my plan while waiting"),
        "in-flight assistant text must render live, not only once flushed: {rendered:?}"
    );
}

/// Trace the full steer lifecycle visually: stream-1 live → steer queued →
/// stream-1 done → stream-2 live.  Shows what the screen looks like at each
/// phase so we can confirm the expected ordering.
///
/// Each phase's `TuiState` is built directly (the exact result a reducer
/// would have produced) rather than driven through one, since this test
/// targets `tui_widgets::draw_chat`'s rendering order, not state-transition logic
/// (which is covered by `AppState`/`presenter.rs` tests).
#[test]
fn steer_render_order_across_full_steer_cycle() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::AgentMode;

    let render = |state: &mut ylva::TuiState| -> Vec<String> {
        let backend = TestBackend::new(60, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut textarea = TextArea::default();
        terminal
            .draw(|frame| {
                tui_widgets::draw(
                    frame,
                    state,
                    &mut textarea,
                    None,
                    None,
                    &ylva::ToolRegistry::new(),
                );
            })
            .expect("draw");
        let buf = terminal.backend().buffer().clone();
        (0..24u16)
            .map(|y| {
                (0..60u16)
                    .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                    .collect::<String>()
            })
            .collect()
    };

    // Phase 1: stream-1 live, no steer yet — "stream1" should be visible.
    let mut state = ylva::TuiState {
        chat_entries: vec![ChatEntry::UserMessage {
            text: "hello".to_string(),
            state: UserMessageState::Sent,
        }],
        partial_assistant_text: "stream1".to_string(),
        agent_mode: AgentMode::Streaming,
        ..ylva::TuiState::new()
    };
    let rows = render(&mut state);
    assert!(
        rows.iter().any(|r| r.contains("stream1")),
        "stream1 visible"
    );

    // Phase 2: steer queued during stream-1.
    state.queued_steer_texts.push("steer".to_string());
    let rows = render(&mut state);
    let stream_row = rows
        .iter()
        .position(|r| r.contains("stream1"))
        .expect("stream1");
    let steer_row = rows
        .iter()
        .position(|r| r.contains("steer"))
        .expect("steer");
    assert!(
        steer_row > stream_row,
        "phase2: steer (row {steer_row}) must be BELOW stream1 (row {stream_row})"
    );

    // Phase 3: stream-1 done → steer applied → stream-2 starts. This is what
    // `flush_partial_assistant_message` + `commit_queued_steer_messages`
    // would have produced: the committed AssistantMessage sits between the
    // original user message and the (now Steered) steer bubble.
    state.queued_steer_texts.clear();
    state.chat_entries = vec![
        ChatEntry::UserMessage {
            text: "hello".to_string(),
            state: UserMessageState::Sent,
        },
        ChatEntry::AssistantMessage {
            text: "stream1".to_string(),
            interrupted: false,
        },
        ChatEntry::UserMessage {
            text: "steer".to_string(),
            state: UserMessageState::Steered,
        },
    ];
    state.partial_assistant_text = "stream2".to_string();
    let rows = render(&mut state);
    let stream1_row = rows
        .iter()
        .position(|r| r.contains("stream1"))
        .expect("stream1 committed");
    let steer_row = rows
        .iter()
        .position(|r| r.contains("steer"))
        .expect("steer committed");
    let stream2_row = rows
        .iter()
        .position(|r| r.contains("stream2"))
        .expect("stream2");
    assert!(
        stream1_row < steer_row,
        "phase3: stream1 (row {stream1_row}) before steer (row {steer_row})"
    );
    assert!(
        steer_row < stream2_row,
        "phase3: steer (row {steer_row}) before stream2 (row {stream2_row})"
    );
}

/// Message sent while `submitted == true` (provider has message-a but
/// StreamStarted has not yet arrived) must be treated as Queued so the
/// deferral logic keeps it below the eventual streaming bubble.
#[tokio::test]
async fn message_sent_while_submitted_is_queued_not_sent() {
    let run = GatedTuiRun::new(None);
    type_and_enter(&run.actions, "message-a").await;
    run.wait_for_request().await;
    run.wait_for_text("message-a").await;
    type_and_enter(&run.actions, "message-b").await;
    run.wait_for_text("1 message queued").await;
    run.provider.release_request.add_permits(1);
    run.wait_for_text("answer-1").await;
    run.provider.release_response.add_permits(1);
    run.start_response(2).await;
    run.finish_response(2).await;
    let state = run.close().await;

    // "message-b" must have been seen as Queued at some point; after the full
    // cycle it transitions to Steered (applied) rather than staying Sent.
    // More importantly the data order must be:
    //   UserMessage(a), AssistantMessage(a), UserMessage(b), AssistantMessage(b)
    let entries = &state.chat_entries;
    assert!(
        entries.len() >= 4,
        "expected ≥4 entries, got {}: {:?}",
        entries.len(),
        entries
    );

    assert!(
        matches!(&entries[0], ChatEntry::UserMessage { text, .. } if text == "message-a"),
        "entry[0] should be user message-a"
    );
    assert!(
        matches!(&entries[1], ChatEntry::AssistantMessage { .. }),
        "entry[1] should be assistant response to a"
    );
    assert!(
        matches!(&entries[2], ChatEntry::UserMessage { text, .. } if text == "message-b"),
        "entry[2] should be user message-b, got {:?}",
        entries[2]
    );
    assert!(
        matches!(&entries[3], ChatEntry::AssistantMessage { .. }),
        "entry[3] should be assistant response to b"
    );
}

#[test]
fn pending_tool_bubble_renders_after_completed_entries() {
    // A ToolBubble in Pending state must be deferred to the end of the
    // rendered conversation so it sits close to the approve/deny footer,
    // even when there are earlier completed entries above it.
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::ToolCallState;

    let backend = TestBackend::new(60, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");

    // An assistant message comes first, then a tool is requested.
    let tool_id = uuid::Uuid::new_v4();
    let mut state = ylva::TuiState {
        chat_entries: vec![
            ChatEntry::UserMessage {
                text: "run something".to_string(),
                state: UserMessageState::Sent,
            },
            ChatEntry::AssistantMessage {
                text: "I will use a tool".to_string(),
                interrupted: false,
            },
            ChatEntry::ToolBubble {
                id: tool_id,
                name: "glob".to_string(),
                state: Box::new(ToolCallState::Pending {
                    arguments: serde_json::json!({"pattern": "**/*.rs"}),
                    content_sections: vec![],
                    title_arguments: vec![],
                }),
                collapsed: true,
            },
        ],
        // The approve/deny footer (drawn only when `pending_approval` is
        // `Some`) is where the lowercase tool name actually renders; the
        // bubble title itself shows the capitalised form ("Glob").
        pending_approval: Some((
            tool_id,
            "glob".to_string(),
            serde_json::json!({"pattern": "**/*.rs"}),
        )),
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
    let rows: Vec<String> = (0..30u16)
        .map(|y| {
            (0..60u16)
                .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                .collect::<String>()
        })
        .collect();

    let assistant_row = rows
        .iter()
        .position(|r| r.contains("I will use a tool"))
        .expect("assistant text should be visible");
    let tool_row = rows
        .iter()
        .position(|r| r.contains("glob"))
        .expect("pending tool bubble should be visible");

    assert!(
        tool_row > assistant_row,
        "pending tool bubble (row {tool_row}) should render BELOW assistant text (row {assistant_row})"
    );

    // The tool bubble state in chat_entries must still be Pending.
    let has_pending = state.chat_entries.iter().any(|entry| {
        matches!(
            entry,
            ChatEntry::ToolBubble {
                state,
                ..
            } if matches!(state.as_ref(), ToolCallState::Pending { .. })
        )
    });
    assert!(
        has_pending,
        "chat_entries should still contain a Pending tool bubble"
    );
}

#[test]
fn expanded_tool_bubble_with_multiline_title_argument_stays_single_line_and_gets_a_section() {
    // A multiline scalar title argument (e.g. edit's `old_string`/`new_string`)
    // must not embed raw newlines into the args line of an expanded tool
    // bubble — that breaks the args line's layout by bleeding into
    // neighbouring key=value pairs. Instead it should render as a
    // placeholder-sanitized single line, with the full text available as its
    // own expandable content section.
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::ToolCallState;

    let backend = TestBackend::new(80, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let tool_id = uuid::Uuid::new_v4();
    let mut state = ylva::TuiState {
        chat_entries: vec![ChatEntry::ToolBubble {
            id: tool_id,
            name: "edit".to_string(),
            state: Box::new(ToolCallState::Running {
                arguments: serde_json::json!({
                    "path": "logs/poem.md",
                    "mode": "search_replace",
                    "old_string": "line one\nline two",
                    "new_string": "replaced one\nreplaced two",
                    "replace_all": true,
                }),
                content_sections: vec![],
                title_arguments: vec![
                    ("path".to_string(), serde_json::json!("logs/poem.md")),
                    ("mode".to_string(), serde_json::json!("search_replace")),
                    (
                        "old_string".to_string(),
                        serde_json::json!("line one\nline two"),
                    ),
                    (
                        "new_string".to_string(),
                        serde_json::json!("replaced one\nreplaced two"),
                    ),
                    ("replace_all".to_string(), serde_json::json!(true)),
                ],
                progress_arguments: vec![],
            }),
            collapsed: false,
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
    let rows: Vec<String> = (0..30u16)
        .map(|y| {
            (0..80u16)
                .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                .collect::<String>()
        })
        .collect();

    // The args line renders "old_string=<value>" as a single sanitized row
    // (word-wrapping to the next row afterwards is fine — what must not
    // happen is a raw embedded newline splitting the *value* itself).
    let args_row = rows
        .iter()
        .position(|r| r.contains("old_string="))
        .expect("args row containing old_string= should be present");
    assert!(
        rows[args_row].contains('↵'),
        "embedded newline should render as a placeholder glyph, got: {:?}",
        rows[args_row]
    );

    // The full multiline value must still be readable, as its own labelled
    // content section in the expanded body.
    assert!(
        rows.iter().any(|r| r.contains("old_string:")),
        "expanded body should contain a labelled old_string content section"
    );
    assert!(
        rows.iter().any(|r| r.contains("line one")) && rows.iter().any(|r| r.contains("line two")),
        "full old_string text should be visible across its own content-section lines"
    );
}

#[test]
fn expanded_bash_bubble_hides_covered_args_and_orders_label_before_command() {
    use ylva::ToolCallState;

    let tool_id = uuid::Uuid::new_v4();
    let mut state = ylva::TuiState {
        chat_entries: vec![ChatEntry::ToolBubble {
            id: tool_id,
            name: "bash".to_string(),
            state: Box::new(ToolCallState::Pending {
                arguments: serde_json::json!({
                    "command": "echo hi",
                    "label": "Greet the user",
                }),
                content_sections: bash_label_command_sections(),
                title_arguments: vec![
                    ("command".to_string(), serde_json::json!("echo hi")),
                    ("label".to_string(), serde_json::json!("Greet the user")),
                ],
            }),
            collapsed: false,
        }],
        ..ylva::TuiState::new()
    };
    let rows = render_tool_bubble_rows(&mut state, 80, 30);

    let label_row = rows
        .iter()
        .position(|row| row.contains("Label:"))
        .expect("Label header should render");
    let command_row = rows
        .iter()
        .position(|row| row.contains("Command:"))
        .expect("Command header should render");
    assert!(
        label_row < command_row,
        "Label section must come before Command: label_row={label_row}, command_row={command_row}"
    );
    assert!(
        !rows.iter().any(|row| row.contains("label=")),
        "covered args line must be hidden when expanded, rows: {rows:?}"
    );
    assert_eq!(
        rows.iter().filter(|row| row.contains("echo hi")).count(),
        1,
        "command must appear exactly once (section body, not args line): {rows:?}"
    );
}

#[test]
fn collapsed_bash_bubble_keeps_command_and_label_in_title() {
    use ylva::ToolCallState;

    let tool_id = uuid::Uuid::new_v4();
    let mut state = ylva::TuiState {
        chat_entries: vec![ChatEntry::ToolBubble {
            id: tool_id,
            name: "bash".to_string(),
            state: Box::new(ToolCallState::Pending {
                arguments: serde_json::json!({
                    "command": "echo hi",
                    "label": "Greet the user",
                }),
                content_sections: bash_label_command_sections(),
                title_arguments: vec![
                    ("command".to_string(), serde_json::json!("echo hi")),
                    ("label".to_string(), serde_json::json!("Greet the user")),
                ],
            }),
            collapsed: true,
        }],
        ..ylva::TuiState::new()
    };
    let rows = render_tool_bubble_rows(&mut state, 80, 30);

    let title_row = rows
        .iter()
        .find(|row| row.contains("Bash"))
        .expect("collapsed title row should render");
    assert!(
        title_row.contains("echo hi"),
        "collapsed title must keep the command primary: {title_row}"
    );
    assert!(
        title_row.contains("label=Greet the user"),
        "collapsed title must keep the label prop: {title_row}"
    );
}

#[test]
fn expanded_bash_multiline_command_renders_one_command_section() {
    use ylva::agent::tools::{ContentSection, Section};
    use ylva::ToolCallState;

    let multiline_command = "echo one\necho two";
    let tool_id = uuid::Uuid::new_v4();
    let mut state = ylva::TuiState {
        chat_entries: vec![ChatEntry::ToolBubble {
            id: tool_id,
            name: "bash".to_string(),
            state: Box::new(ToolCallState::Pending {
                arguments: serde_json::json!({
                    "command": multiline_command,
                    "label": "Two echoes",
                }),
                content_sections: vec![
                    Section::Content(ContentSection {
                        label: Some("Label".to_string()),
                        content: "Two echoes".to_string(),
                        file_extension: None,
                        line_number_offset: None,
                    }),
                    Section::Content(ContentSection {
                        label: Some("Command".to_string()),
                        content: multiline_command.to_string(),
                        file_extension: None,
                        line_number_offset: None,
                    }),
                ],
                title_arguments: vec![
                    ("command".to_string(), serde_json::json!(multiline_command)),
                    ("label".to_string(), serde_json::json!("Two echoes")),
                ],
            }),
            collapsed: false,
        }],
        ..ylva::TuiState::new()
    };
    let rows = render_tool_bubble_rows(&mut state, 80, 30);

    let label_row = rows
        .iter()
        .position(|row| row.contains("Label:"))
        .expect("Label header should render");
    let command_row = rows
        .iter()
        .position(|row| row.contains("Command:"))
        .expect("Command header should render");
    assert!(label_row < command_row, "Label must precede Command");
    assert!(
        !rows.iter().any(|row| row.contains("label=")),
        "covered args line must be hidden for multiline commands too: {rows:?}"
    );
    assert_eq!(
        rows.iter().filter(|row| row.contains("echo one")).count(),
        1,
        "multiline command must render exactly once (dedup guard), rows: {rows:?}"
    );
    assert_eq!(
        rows.iter().filter(|row| row.contains("echo two")).count(),
        1,
        "multiline command must render exactly once (dedup guard), rows: {rows:?}"
    );
}

#[test]
fn expanded_bash_command_section_renders_with_syntax_colors() {
    // The Command section carries `file_extension: "sh"`, so the renderer
    // picks the shell grammar: the command row must contain more than one
    // foreground color (comment token vs default), which the plain-text
    // path (single theme-default fg) never produces.
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::ToolCallState;

    let command = "echo hi # trailing comment";
    let registry = ylva::default_registry(
        std::sync::Arc::new(std::sync::RwLock::new(ylva::Sandbox::new())),
        ylva::ToolRegistryConfig::default(),
    );
    let call = registry
        .create_call(
            "bash",
            uuid::Uuid::new_v4(),
            serde_json::json!({
                "command": command,
                "label": "Greet the user",
            }),
        )
        .expect("bash tool call");
    let content_sections = call.pending_content_sections();

    let tool_id = uuid::Uuid::new_v4();
    let mut state = ylva::TuiState {
        chat_entries: vec![ChatEntry::ToolBubble {
            id: tool_id,
            name: "bash".to_string(),
            state: Box::new(ToolCallState::Pending {
                arguments: serde_json::json!({
                    "command": command,
                    "label": "Greet the user",
                }),
                content_sections,
                title_arguments: vec![
                    ("command".to_string(), serde_json::json!(command)),
                    ("label".to_string(), serde_json::json!("Greet the user")),
                ],
            }),
            collapsed: false,
        }],
        ..ylva::TuiState::new()
    };

    let backend = TestBackend::new(80, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
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

    let rows: Vec<String> = (0..30u16)
        .map(|y| {
            (0..80u16)
                .map(|x| buffer.cell((x, y)).unwrap().symbol().to_string())
                .collect()
        })
        .collect();
    let command_row = rows
        .iter()
        .position(|row| row.contains("trailing comment"))
        .expect("command body row should render");

    // Skip the two-cell bar+margin prefix; ignore spaces between tokens.
    let distinct_foregrounds: std::collections::HashSet<_> = (2..80u16)
        .map(|x| buffer.cell((x, command_row as u16)).unwrap())
        .filter(|cell| cell.symbol() != " ")
        .filter_map(|cell| cell.style().fg)
        .collect();
    assert!(
        distinct_foregrounds.len() >= 2,
        "command row should be syntax-highlighted (>=2 foreground colors), got {distinct_foregrounds:?} — rows: {rows:?}"
    );
}

#[test]
fn expanded_bash_done_bubble_renders_result_header_last() {
    use ylva::tui_reducer_state::ResultSection;
    use ylva::ToolCallState;

    let tool_id = uuid::Uuid::new_v4();
    let mut state = ylva::TuiState {
        chat_entries: vec![ChatEntry::ToolBubble {
            id: tool_id,
            name: "bash".to_string(),
            state: Box::new(ToolCallState::Done {
                arguments: serde_json::json!({
                    "command": "echo hi",
                    "label": "Greet the user",
                }),
                result_section: ResultSection::plain("hi\n"),
                content_sections: vec![
                    ylva::agent::tools::Section::Content(ylva::agent::tools::ContentSection {
                        label: Some("Label".to_string()),
                        content: "Greet the user".to_string(),
                        file_extension: None,
                        line_number_offset: None,
                    }),
                    ylva::agent::tools::Section::Content(ylva::agent::tools::ContentSection {
                        label: Some("Command".to_string()),
                        content: "echo hi".to_string(),
                        file_extension: None,
                        line_number_offset: None,
                    }),
                    ylva::agent::tools::Section::Content(ylva::agent::tools::ContentSection {
                        label: Some("Result".to_string()),
                        content: "hi\n".to_string(),
                        file_extension: None,
                        line_number_offset: Some(0),
                    }),
                ],
                summary: vec![],
                duration: std::time::Duration::from_millis(12),
                title_arguments: vec![
                    ("command".to_string(), serde_json::json!("echo hi")),
                    ("label".to_string(), serde_json::json!("Greet the user")),
                ],
            }),
            collapsed: false,
        }],
        ..ylva::TuiState::new()
    };
    let rows = render_tool_bubble_rows(&mut state, 80, 30);

    let label_row = rows
        .iter()
        .position(|row| row.contains("Label:"))
        .expect("Label header should render");
    let command_row = rows
        .iter()
        .position(|row| row.contains("Command:"))
        .expect("Command header should render");
    let result_row = rows
        .iter()
        .position(|row| row.contains("Result:"))
        .expect("Result header should render");
    assert!(
        label_row < command_row && command_row < result_row,
        "headers must render Label, Command, Result in order: {label_row} {command_row} {result_row}"
    );
    assert!(
        !rows.iter().any(|row| row.contains("label=")),
        "covered args line must stay hidden in Done: {rows:?}"
    );
    assert!(
        rows[result_row..].iter().any(|row| row.contains("hi")),
        "result content should render under the Result header: {rows:?}"
    );
}

#[test]
fn expanded_bubble_shows_args_when_sections_do_not_cover_title_keys() {
    // Data-driven gate: a section whose label doesn't match the title keys
    // (here `write`'s unlabeled content preview) must NOT hide the args line.
    use ylva::agent::tools::{ContentSection, Section};
    use ylva::ToolCallState;

    let tool_id = uuid::Uuid::new_v4();
    let mut state = ylva::TuiState {
        chat_entries: vec![ChatEntry::ToolBubble {
            id: tool_id,
            name: "write".to_string(),
            state: Box::new(ToolCallState::Pending {
                arguments: serde_json::json!({
                    "path": "src/main.rs",
                    "content": "fn main() {}",
                }),
                content_sections: vec![Section::Content(ContentSection {
                    label: None,
                    content: "fn main() {}".to_string(),
                    file_extension: Some("rs".to_string()),
                    line_number_offset: Some(0),
                })],
                title_arguments: vec![("path".to_string(), serde_json::json!("src/main.rs"))],
            }),
            collapsed: false,
        }],
        ..ylva::TuiState::new()
    };
    let rows = render_tool_bubble_rows(&mut state, 80, 30);

    assert!(
        rows.iter().any(|row| row.contains("src/main.rs")),
        "args line must still show the uncovered title key, rows: {rows:?}"
    );
}

#[test]
fn collapsed_bubble_caps_title_args_with_the_ellipsis_glyph() {
    use ylva::tui_reducer_state::ResultSection;
    use ylva::ToolCallState;

    // Long enough that the collapsed title must cut it at any sane width.
    let long_terms = "agent-harnesses that can be run on MacOS with a very long query ".repeat(4);
    let mut state = ylva::TuiState {
        chat_entries: vec![ChatEntry::ToolBubble {
            id: uuid::Uuid::new_v4(),
            name: "web_search".to_string(),
            state: Box::new(ToolCallState::Done {
                arguments: serde_json::json!({ "terms": long_terms }),
                result_section: ResultSection::plain(""),
                content_sections: vec![],
                summary: vec![],
                duration: std::time::Duration::from_millis(1234),
                title_arguments: vec![("terms".to_string(), serde_json::json!(long_terms))],
            }),
            collapsed: true,
        }],
        ..ylva::TuiState::new()
    };

    let rows = render_tool_bubble_rows(&mut state, 60, 20);
    let title_row = rows
        .iter()
        .find(|row| row.contains("Web Search"))
        .expect("the collapsed title row renders");

    assert!(
        title_row.contains('…'),
        "capped title args must end in the ellipsis glyph: {title_row:?}"
    );
    assert!(
        !title_row.contains("..."),
        "three plain dots must not be used as the cap marker: {title_row:?}"
    );
    assert!(
        !title_row.contains(" …"),
        "the cut must not leave a space before the marker: {title_row:?}"
    );
    assert!(
        title_row.contains('→'),
        "the duration summary stays visible after the marker: {title_row:?}"
    );
    assert!(
        title_row.contains("… → 1.234s"),
        "the marker is followed by the duration summary: {title_row:?}"
    );
}

#[test]
fn collapsed_bubble_title_cut_drops_a_space_before_the_marker() {
    use ylva::tui_reducer_state::ResultSection;
    use ylva::ToolCallState;

    // Word followed by enough spaces that any realistic cut lands inside the
    // run — without trimming, the marker would render as `word     …`.
    let long_terms = format!("agent-harnesses{}", " ".repeat(400));
    let mut state = ylva::TuiState {
        chat_entries: vec![ChatEntry::ToolBubble {
            id: uuid::Uuid::new_v4(),
            name: "web_search".to_string(),
            state: Box::new(ToolCallState::Done {
                arguments: serde_json::json!({ "terms": long_terms }),
                result_section: ResultSection::plain(""),
                content_sections: vec![],
                summary: vec![],
                duration: std::time::Duration::from_millis(1234),
                title_arguments: vec![("terms".to_string(), serde_json::json!(long_terms))],
            }),
            collapsed: true,
        }],
        ..ylva::TuiState::new()
    };

    let rows = render_tool_bubble_rows(&mut state, 120, 20);
    let title_row = rows
        .iter()
        .find(|row| row.contains("agent-harnesses"))
        .expect("the collapsed title row renders");

    assert!(
        title_row.contains("agent-harnesses…"),
        "the marker must hug the last visible character: {title_row:?}"
    );
    assert!(
        !title_row.contains(" …"),
        "the cut must not leave whitespace before the marker: {title_row:?}"
    );
}

#[tokio::test]
async fn escape_twice_while_streaming_stops_stream_but_keeps_tui_open() {
    // Esc+Esc must stop the agent but NOT exit the TUI (unlike Ctrl+C+Ctrl+C).
    let run = GatedTuiRun::new(None);
    type_and_enter(&run.actions, "hello").await;
    run.start_response(1).await;
    send(&run.actions, AppAction::Escape).await;
    run.wait_for_text("Press esc again").await;
    send(&run.actions, AppAction::Escape).await;
    run.wait_for_text("Idle").await;

    // TUI must still be running after Esc+Esc.
    assert!(
        !run.handle.is_finished(),
        "TUI must remain open after Esc+Esc"
    );
    let state = run.close().await;
    assert_eq!(state.agent_mode, ylva::AgentMode::Idle);
}

#[test]
fn startup_logo_shows_selected_profile() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::{tui_widgets, TuiDisplayMetadata, TuiState};

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut state = TuiState {
        display_metadata: TuiDisplayMetadata {
            profile_name: "work".to_string(),
            ..TuiDisplayMetadata::default()
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
    let rendered: String = (0..24u16)
        .flat_map(|row| (0..80u16).map(move |column| (column, row)))
        .map(|(column, row)| buffer.cell((column, row)).unwrap().symbol().to_string())
        .collect();
    assert!(
        rendered.contains("profile: work"),
        "startup logo should show the selected profile: {rendered:?}"
    );
}
