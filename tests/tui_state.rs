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

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

use ylva::tui_driver::NoopClipboard;
use ylva::{tui_driver, AppAction, ChatEntry, MockProvider, ToolRegistry, UserMessageState};

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
            tui_driver::run_tui(
                &mut terminal,
                Arc::new(provider),
                registry,
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
            tui_driver::run_tui(
                &mut terminal,
                Arc::new(provider),
                registry,
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
async fn key_agnostic_ctrl_c_then_escape_stops_stream() {
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
            tui_driver::run_tui(
                &mut terminal,
                Arc::new(provider),
                registry,
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
    send(&action_tx, AppAction::CtrlC).await;
    send(&action_tx, AppAction::Escape).await; // second press (key-agnostic) → Stop
    tokio::time::sleep(Duration::from_millis(120)).await;
    drop(action_tx);

    let state = run_handle.await.expect("task joined");
    assert_eq!(state.agent_mode, ylva::AgentMode::Idle);
    assert!(
        state.chat_entries.iter().any(|entry| matches!(
            entry,
            ChatEntry::UserMessage {
                state: UserMessageState::Cancelled,
                ..
            }
        )),
        "queued steer should be cancelled"
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
            tui_driver::run_tui(
                &mut terminal,
                Arc::new(provider),
                registry,
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
            tui_driver::run_tui(
                &mut terminal,
                Arc::new(provider),
                registry,
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
            tui_driver::run_tui(
                &mut terminal,
                Arc::new(provider),
                registry,
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
    use ylva::ui;
    use ylva::UiUpdate;

    let backend = TestBackend::new(60, 20);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut state = ylva::TuiState::new();
    let mut textarea = TextArea::default();

    state.push_user_bubble("hello".to_string(), UserMessageState::Sent);
    state.apply_update(&UiUpdate::StreamStarted);
    state.apply_update(&UiUpdate::AppendText("world".to_string()));
    state.push_user_bubble("steer me".to_string(), UserMessageState::Queued);

    terminal
        .draw(|frame| {
            ui::draw(frame, &mut state, &mut textarea, None, None);
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
    use ylva::{ui, UiUpdate};

    let backend = TestBackend::new(60, 20);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut state = ylva::TuiState::new();
    let mut textarea = TextArea::default();

    // Stream started but no text yet; user already queued a steer.
    state.push_user_bubble("hello".to_string(), UserMessageState::Sent);
    state.apply_update(&UiUpdate::StreamStarted);
    // Steer arrives before any AppendText chunk.
    state.push_user_bubble("steer me".to_string(), UserMessageState::Queued);

    terminal
        .draw(|frame| {
            ui::draw(frame, &mut state, &mut textarea, None, None);
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
    state.apply_update(&UiUpdate::AppendText("world".to_string()));
    terminal
        .draw(|frame| {
            ui::draw(frame, &mut state, &mut textarea, None, None);
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

/// Trace the full steer lifecycle visually: stream-1 live → steer queued →
/// stream-1 done → stream-2 live.  Shows what the screen looks like at each
/// phase so we can confirm the expected ordering.
#[test]
fn steer_render_order_across_full_steer_cycle() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::{ui, UiUpdate};

    let render = |state: &mut ylva::TuiState| -> Vec<String> {
        let backend = TestBackend::new(60, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut textarea = TextArea::default();
        terminal
            .draw(|frame| {
                ui::draw(frame, state, &mut textarea, None, None);
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

    let mut state = ylva::TuiState::new();
    state.push_user_bubble("hello".to_string(), UserMessageState::Sent);
    state.apply_update(&UiUpdate::StreamStarted);
    state.apply_update(&UiUpdate::AppendText("stream1".to_string()));

    // Phase 1: stream-1 live, no steer yet — "stream1" should be visible.
    let rows = render(&mut state);
    assert!(
        rows.iter().any(|r| r.contains("stream1")),
        "stream1 visible"
    );

    // Phase 2: steer queued during stream-1.
    state.push_user_bubble("steer".to_string(), UserMessageState::Queued);
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

    // Phase 3: stream-1 done → steer applied → stream-2 starts.
    state.apply_update(&UiUpdate::StreamDone);
    state.apply_update(&UiUpdate::SteerApplied);
    state.apply_update(&UiUpdate::StreamStarted);
    state.apply_update(&UiUpdate::AppendText("stream2".to_string()));
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
            ylva::tui_driver::run_tui(
                &mut terminal,
                std::sync::Arc::new(provider),
                registry,
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
    use ylva::ui;
    use ylva::{ToolCallState, UiUpdate};

    let backend = TestBackend::new(60, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut state = ylva::TuiState::new();
    let mut textarea = TextArea::default();

    // An assistant message comes first, then a tool is requested.
    state.push_user_bubble("run something".to_string(), UserMessageState::Sent);
    state.apply_update(&UiUpdate::StreamStarted);
    state.apply_update(&UiUpdate::AppendText("I will use a tool".to_string()));
    state.apply_update(&UiUpdate::StreamDone);

    let tool_id = uuid::Uuid::new_v4();
    state.apply_update(&UiUpdate::ToolPending {
        id: tool_id,
        name: "glob".to_string(),
        arguments: serde_json::json!({"pattern": "**/*.rs"}),
        content_sections: vec![],

        title_arguments: vec![],
    });

    terminal
        .draw(|frame| {
            ui::draw(frame, &mut state, &mut textarea, None, None);
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
                state: ToolCallState::Pending { .. },
                ..
            }
        )
    });
    assert!(
        has_pending,
        "chat_entries should still contain a Pending tool bubble"
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
            tui_driver::run_tui(
                &mut terminal,
                Arc::new(provider),
                registry,
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
