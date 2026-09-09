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

mod common;

use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

use ylva::tui_driver::NoopClipboard;
use ylva::{
    tui_driver, AgentLoop, AppAction, ChatEntry, EngineImpl, MockProvider, ToolRegistry,
    UserMessageState,
};

/// Type `text` into the (driver-owned) textarea and press Enter.
async fn type_and_enter(tx: &mpsc::Sender<AppAction>, text: &str) {
    for ch in text.chars() {
        tx.send(AppAction::KeyInput(KeyEvent::new(
            KeyCode::Char(ch),
            KeyModifiers::NONE,
        )))
        .await
        .expect("action channel open");
    }
    tx.send(AppAction::KeyInput(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )))
    .await
    .expect("action channel open");
}

async fn send(tx: &mpsc::Sender<AppAction>, action: AppAction) {
    tx.send(action).await.expect("action channel open");
}

fn is_assistant_message(entry: &ChatEntry) -> bool {
    matches!(entry, ChatEntry::AssistantMessage { .. })
}

#[tokio::test]
async fn typed_enter_creates_sent_user_bubble_and_assistant() {
    let provider = MockProvider::new(vec!["hello".to_string()], Duration::from_millis(5));
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

    type_and_enter(&action_tx, "hi").await;
    tokio::time::sleep(Duration::from_millis(80)).await;
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
    let provider = MockProvider::new(
        vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ],
        Duration::from_millis(40),
    );
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

    type_and_enter(&action_tx, "first").await;
    tokio::time::sleep(Duration::from_millis(50)).await; // streaming
    type_and_enter(&action_tx, "steer").await; // queued
    send(&action_tx, AppAction::CtrlC).await; // first press
    send(&action_tx, AppAction::CtrlC).await; // second press → Stop
    tokio::time::sleep(Duration::from_millis(120)).await; // let StreamCancelled drain
    drop(action_tx);

    let state = run_handle.await.expect("task joined");

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
    let provider = MockProvider::new(
        vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ],
        Duration::from_millis(40),
    );
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

    type_and_enter(&action_tx, "first").await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    type_and_enter(&action_tx, "steer").await;
    send(&action_tx, AppAction::CtrlC).await; // arms ctrl-c's confirm
    send(&action_tx, AppAction::Escape).await; // mismatched key: must not stop
                                               // Wait for both turns to complete naturally.
    tokio::time::sleep(Duration::from_millis(400)).await;
    drop(action_tx);

    let state = run_handle.await.expect("task joined");
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
    let provider = MockProvider::new(
        vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ],
        Duration::from_millis(30),
    );
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

    type_and_enter(&action_tx, "first").await;
    tokio::time::sleep(Duration::from_millis(50)).await; // first stream in flight
    type_and_enter(&action_tx, "steer").await; // queued
                                               // Wait for: first stream done → steer injected (SteerApplied) → second
                                               // stream completes → idle.
    tokio::time::sleep(Duration::from_millis(400)).await;
    drop(action_tx);

    let state = run_handle.await.expect("task joined");
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
    let provider = MockProvider::new(vec!["ok".to_string()], Duration::from_millis(5));
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
                Some("init".to_string()),
                action_rx,
                &mut NoopClipboard,
            )
            .await
            .expect("run_tui completed")
        }
    });

    tokio::time::sleep(Duration::from_millis(80)).await;
    drop(action_tx);

    let state = run_handle.await.expect("task joined");
    assert_eq!(
        state.chat_entries[0],
        ChatEntry::UserMessage {
            text: "init".to_string(),
            state: UserMessageState::Sent
        }
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

    // Simulate a mouse-down in the chat area (row 5, column 5).
    send(
        &action_tx,
        AppAction::MouseInput(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 5,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }),
    )
    .await;

    // Simulate a drag to column 15 on the same row.
    send(
        &action_tx,
        AppAction::MouseInput(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 15,
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
            column: 15,
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
            tui_widgets::draw(frame, &mut state, &mut textarea, None, None);
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
            tui_widgets::draw(frame, &mut state, &mut textarea, None, None);
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
            tui_widgets::draw(frame, &mut state, &mut textarea, None, None);
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
            tui_widgets::draw(frame, &mut state, &mut textarea, None, None);
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
            tui_widgets::draw(frame, &mut state, &mut textarea, None, None);
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
fn footer_shows_the_active_provider_and_model() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::{EngineDisplayInfo, TuiState};

    let backend = TestBackend::new(60, 20);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut state = TuiState {
        engine_display: EngineDisplayInfo {
            provider_connection: "ollama".to_string(),
            model: "llama3.2".to_string(),
            reasoning_effort: Some("max".to_string()),
        },
        ..TuiState::new()
    };
    let mut textarea = TextArea::default();

    terminal
        .draw(|frame| {
            tui_widgets::draw(frame, &mut state, &mut textarea, None, None);
        })
        .expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let rendered: String = (0..20u16)
        .flat_map(|row| (0..60u16).map(move |column| (column, row)))
        .map(|(column, row)| buffer.cell((column, row)).unwrap().symbol().to_string())
        .collect();
    assert!(
        rendered.contains("ollama/llama3.2 (max)"),
        "footer must show the active provider/model identity: {rendered:?}"
    );

    state.engine_display.reasoning_effort = None;
    terminal
        .draw(|frame| {
            tui_widgets::draw(frame, &mut state, &mut textarea, None, None);
        })
        .expect("draw without explicit reasoning effort");

    let buffer = terminal.backend().buffer().clone();
    let rendered_without_reasoning: String = (0..20u16)
        .flat_map(|row| (0..60u16).map(move |column| (column, row)))
        .map(|(column, row)| buffer.cell((column, row)).unwrap().symbol().to_string())
        .collect();
    assert!(
        rendered_without_reasoning.contains("ollama/llama3.2"),
        "footer must retain the active provider/model identity: {rendered_without_reasoning:?}"
    );
    assert!(
        !rendered_without_reasoning.contains("ollama/llama3.2 ("),
        "footer must omit the default reasoning effort: {rendered_without_reasoning:?}"
    );
}

#[test]
fn model_picker_footer_shows_picker_controls_instead_of_send() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_reducer_state::{ModelChoice, SlashOverlay};
    use ylva::tui_widgets;
    use ylva::{ModelInfo, ProviderModelOption, TuiState};

    let backend = TestBackend::new(80, 20);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut state = TuiState {
        slash_overlay: SlashOverlay::ModelPicker {
            choices: vec![ModelChoice {
                label: "stub/thinking  reasoning: default ⚙".to_string(),
                selection: ProviderModelOption {
                    provider_connection: "stub".to_string(),
                    model: ModelInfo {
                        id: "thinking".to_string(),
                        context_length: None,
                        reasoning_efforts: Some(vec!["low".to_string(), "high".to_string()]),
                    },
                    reasoning_effort: None,
                },
                is_active: false,
            }],
            filter: String::new(),
            selected: 0,
        },
        ..TuiState::new()
    };
    let mut textarea = TextArea::default();

    terminal
        .draw(|frame| {
            tui_widgets::draw(frame, &mut state, &mut textarea, None, None);
        })
        .expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let rendered: String = (0..20u16)
        .flat_map(|row| (0..80u16).map(move |column| (column, row)))
        .map(|(column, row)| buffer.cell((column, row)).unwrap().symbol().to_string())
        .collect();
    assert!(
        rendered.contains("←/→: effort"),
        "rendered footer: {rendered:?}"
    );
    assert!(
        rendered.contains("enter: switch"),
        "rendered footer: {rendered:?}"
    );
    assert!(
        rendered.contains("esc: cancel"),
        "rendered footer: {rendered:?}"
    );
    assert!(
        rendered.contains("Provider Models"),
        "model picker must use the provider models title: {rendered:?}"
    );
    assert!(
        !rendered.contains("adjust reasoning"),
        "model picker title must not duplicate footer controls: {rendered:?}"
    );
    assert!(
        !rendered.contains("enter: send"),
        "model picker must not advertise sending: {rendered:?}"
    );
}

/// A live filter that matches nothing must not make the picker vanish
/// entirely — the panel stays open (so the user can keep editing the
/// filter) and shows a message explaining why nothing is listed.
#[test]
fn model_picker_shows_a_message_instead_of_vanishing_when_the_filter_matches_nothing() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_reducer_state::{ModelChoice, SlashOverlay};
    use ylva::tui_widgets;
    use ylva::{ModelInfo, ProviderModelOption, TuiState};

    let backend = TestBackend::new(80, 20);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut state = TuiState {
        slash_overlay: SlashOverlay::ModelPicker {
            choices: vec![ModelChoice {
                label: "stub/thinking  reasoning: default".to_string(),
                selection: ProviderModelOption {
                    provider_connection: "stub".to_string(),
                    model: ModelInfo {
                        id: "thinking".to_string(),
                        context_length: None,
                        reasoning_efforts: None,
                    },
                    reasoning_effort: None,
                },
                is_active: false,
            }],
            filter: "no-such-model".to_string(),
            selected: 0,
        },
        ..TuiState::new()
    };
    let mut textarea = TextArea::default();

    terminal
        .draw(|frame| {
            tui_widgets::draw(frame, &mut state, &mut textarea, None, None);
        })
        .expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let rendered: String = (0..20u16)
        .flat_map(|row| (0..80u16).map(move |column| (column, row)))
        .map(|(column, row)| buffer.cell((column, row)).unwrap().symbol().to_string())
        .collect();
    assert!(
        rendered.contains("Nothing matched your model filter."),
        "the panel must explain why it is empty instead of vanishing: {rendered:?}"
    );
    assert!(
        rendered.contains("Provider Models"),
        "the panel must stay open under its usual title: {rendered:?}"
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
            tui_widgets::draw(frame, &mut state, &mut textarea, None, None);
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
            tui_widgets::draw(frame, &mut state, &mut textarea, None, None);
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
            tui_widgets::draw(frame, &mut state, &mut textarea, None, None);
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
            tui_widgets::draw(frame, &mut state, &mut textarea, None, None);
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

/// The child-view header re-renders the root `spawn_subagent` `ToolBubble`
/// with a shortened `"Subagent"` title instead of the plain title-cased
/// `"Spawn Subagent"` — the header sits directly above the subagent's own
/// transcript, so "Spawn" only makes sense from the root conversation's
/// point of view (regression test for the header always reading "Spawn
/// Subagent ..." regardless of context).
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
            tui_widgets::draw(frame, &mut state, &mut textarea, None, None);
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
            tui_widgets::draw(frame, &mut state, &mut textarea, None, None);
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
    state.agent_mode = AgentMode::Idle;
    terminal
        .draw(|frame| {
            tui_widgets::draw(frame, &mut state, &mut textarea, None, None);
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
                tui_widgets::draw(frame, state, &mut textarea, None, None);
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
    use std::time::Duration;
    // Slow provider: first chunk arrives after 150 ms, giving us time to
    // type the second message before StreamStarted fires.
    let provider = ylva::MockProvider::new(
        vec!["response-a".to_string(), "response-b".to_string()],
        Duration::from_millis(150),
    );
    let backend = ratatui::backend::TestBackend::new(80, 24);
    let terminal = ratatui::Terminal::new(backend).expect("terminal");
    let (action_tx, action_rx) = tokio::sync::mpsc::channel(64);
    let registry = std::sync::Arc::new(ylva::ToolRegistry::new());

    let run_handle = tokio::spawn({
        let mut terminal = terminal;
        async move {
            let engine = std::sync::Arc::new(ylva::EngineImpl::new(
                std::sync::Arc::new(provider),
                common::test_engine_config(),
            ));
            let (agent, ui_rx) = ylva::AgentLoop::new(engine, registry);
            ylva::tui_driver::run_tui(
                &mut terminal,
                agent,
                ui_rx,
                None,
                action_rx,
                &mut NoopClipboard,
            )
            .await
            .expect("run_tui")
        }
    });

    // Send "message-a"; provider delays before streaming so agent_mode stays
    // Idle / submitted for a moment.
    type_and_enter(&action_tx, "message-a").await;
    // Small pause — enough for the Prompt to reach the agent but not enough
    // for StreamStarted to arrive.
    tokio::time::sleep(Duration::from_millis(20)).await;
    // Send "message-b" while submitted==true, agent_mode==Idle.
    type_and_enter(&action_tx, "message-b").await;
    // Wait for both rounds to complete.
    tokio::time::sleep(Duration::from_millis(800)).await;
    drop(action_tx);

    let state = run_handle.await.expect("join");

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
            tui_widgets::draw(frame, &mut state, &mut textarea, None, None);
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
            tui_widgets::draw(frame, &mut state, &mut textarea, None, None);
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

#[tokio::test]
async fn escape_twice_while_streaming_stops_stream_but_keeps_tui_open() {
    // Esc+Esc must stop the agent but NOT exit the TUI (unlike Ctrl+C+Ctrl+C).
    let provider = MockProvider::new(
        vec!["a".to_string(), "b".to_string(), "c".to_string()],
        Duration::from_millis(40),
    );
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

    type_and_enter(&action_tx, "hello").await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    send(&action_tx, AppAction::Escape).await; // first press: enter confirm
    send(&action_tx, AppAction::Escape).await; // second press: send Stop, stay in TUI
    tokio::time::sleep(Duration::from_millis(120)).await;

    // TUI must still be running after Esc+Esc.
    assert!(
        !run_handle.is_finished(),
        "TUI must remain open after Esc+Esc"
    );
    drop(action_tx);

    let state = run_handle.await.expect("task joined");
    assert_eq!(state.agent_mode, ylva::AgentMode::Idle);
}

#[tokio::test]
async fn click_inside_help_popup_keeps_it_open() {
    let provider = MockProvider::new(vec!["hello".to_string()], Duration::from_millis(200));
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

    send(&action_tx, AppAction::ShowHelp).await;
    tokio::time::sleep(Duration::from_millis(50)).await; // let the popup render, setting its rect

    // Click well inside the popup (80x24 terminal → popup spans x:10..70, y:4..20).
    send(
        &action_tx,
        AppAction::MouseInput(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 40,
            row: 10,
            modifiers: KeyModifiers::NONE,
        }),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(action_tx);

    let state = run_handle.await.expect("task joined");
    assert!(
        state.help_popup.show_help,
        "click inside the help popup must not close it"
    );
}

#[tokio::test]
async fn click_outside_help_popup_closes_it() {
    let provider = MockProvider::new(vec!["hello".to_string()], Duration::from_millis(200));
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

    send(&action_tx, AppAction::ShowHelp).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Click outside the popup (top-left corner is outside the centered popup).
    send(
        &action_tx,
        AppAction::MouseInput(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        }),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(action_tx);

    let state = run_handle.await.expect("task joined");
    assert!(
        !state.help_popup.show_help,
        "click outside the help popup must close it"
    );
}
