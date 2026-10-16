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
fn large_plain_and_markdown_buffers_match_legacy_at_every_scroll_position() {
    let markdown = [
        "| Name | Value |",
        "| --- | --- |",
        "| 日本語 | **bold** |",
        "",
        "- list item",
        "  - nested item",
        "",
        "```rust",
        "fn main() { println!(\"hello\"); }",
        "```",
        "",
    ];
    for count in [3_000, 30_000] {
        for markdown_content in [false, true] {
            let content = (0..count)
                .map(|index| {
                    if markdown_content {
                        markdown[index % markdown.len()].to_owned()
                    } else {
                        format!("plain result row {index}\n")
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            let entries = vec![
                completed_render_fixture(content),
                ChatEntry::Notice {
                    text: "end of transcript".into(),
                },
            ];
            let mut state = ylva::AppState::new();
            state.enable_render_cache();
            state.chat_entries = entries.clone();
            for width in [50, 120] {
                for offset in [0, 1_500, usize::MAX] {
                    state.conversation.scroll_locked = false;
                    state.conversation.scroll_offset = offset;
                    let actual = render_offline(&mut state, width, 40);
                    let mut legacy = ylva::TuiState::new();
                    legacy.chat_entries = entries.clone();
                    legacy.conversation.scroll_locked = false;
                    legacy.conversation.scroll_offset = offset;
                    let mut terminal = Terminal::new(TestBackend::new(width, 40)).unwrap();
                    let mut textarea = ratatui_textarea::TextArea::default();
                    terminal
                        .draw(|frame| {
                            ylva::tui_widgets::draw(
                                frame,
                                &mut legacy,
                                &mut textarea,
                                None,
                                None,
                                &ToolRegistry::new(),
                            );
                        })
                        .unwrap();
                    assert_eq!(&actual, terminal.backend().buffer(), "count={count}, markdown={markdown_content}, width={width}, offset={offset}");
                    assert_eq!(
                        state.conversation.scroll_offset,
                        legacy.conversation.scroll_offset
                    );
                }
            }
        }
    }
}

#[test]
fn borrowed_projection_preserves_streaming_and_queued_footer_labels() {
    let mut state = ylva::AppState::new();
    state.enable_render_cache();
    present_render_event(&mut state, ylva::DomainEvent::StreamStarted);
    present_render_event(
        &mut state,
        ylva::DomainEvent::StreamThinkingChunk("reasoning".into()),
    );
    state.queue_steer_message("queued text".into());
    present_render_event(&mut state, ylva::DomainEvent::SteerQueued { run_id: None });
    let text = rendered_text(&render_offline(&mut state, 120, 40));
    assert!(text.contains("Thinking…"));
    assert!(text.contains("1 message queued"));
    present_render_event(&mut state, ylva::DomainEvent::StreamChunk("answer".into()));
    let text = rendered_text(&render_offline(&mut state, 120, 40));
    assert!(text.contains("Streaming…"));
    assert!(text.contains("1 message queued"));
}

#[test]
fn same_length_tool_replacement_preserves_public_compatibility() {
    let mut state = ylva::AppState::new();
    state.enable_render_cache();
    state
        .chat_entries
        .push(completed_render_fixture("old".into()));
    let ChatEntry::ToolBubble { id, .. } = state.chat_entries[0] else {
        unreachable!()
    };
    render_offline(&mut state, 80, 40);
    let Some(ChatEntry::ToolBubble { state: bubble, .. }) = state.find_tool_bubble_mut(id) else {
        unreachable!()
    };
    let ylva::ToolCallState::Done { result_section, .. } = bubble.as_mut() else {
        unreachable!()
    };
    result_section.content = "new".into();
    assert!(rendered_text(&render_offline(&mut state, 80, 40)).contains("new"));
    let mut direct = ylva::AppState::new();
    direct.chat_entries = vec![ChatEntry::Notice { text: "old".into() }];
    render_offline(&mut direct, 80, 40);
    direct.chat_entries[0] = ChatEntry::Notice { text: "new".into() };
    assert!(rendered_text(&render_offline(&mut direct, 80, 40)).contains("new"));
}

#[test]
fn wrapped_document_bottom_scroll_does_not_narrow_absolute_offsets() {
    let mut state = ylva::AppState::new();
    state.enable_render_cache();
    state.chat_entries.push(ChatEntry::Notice {
        text: (0..35_000)
            .map(|row| format!("row {row:05} long content wrapping\n"))
            .collect(),
    });
    let buffer = render_offline(&mut state, 24, 12);
    assert!(state.conversation.scroll_offset > u16::MAX as usize);
    assert!(rendered_text(&buffer).contains("34999"));
    state.conversation.chat_selection = Some(ylva::ChatSelection {
        anchor_line: 1,
        anchor_col: 5,
        cursor_line: state.conversation.chat_rendered_lines.len() - 1,
        cursor_col: 10,
    });
    render_offline(&mut state, 24, 12);
    let text = state.extract_chat_selection_text().unwrap();
    assert!(text.starts_with("row 00000"));
    assert!(text.contains("34999"));
    state.conversation.chat_drag_active = true;
    let effects = ylva::presenter::update(
        &mut state,
        ylva::TuiEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 10,
            row: 0,
            modifiers: KeyModifiers::NONE,
        }),
        &empty_textarea_context(),
    );
    assert!(effects
        .iter()
        .any(|effect| matches!(effect, ylva::Effect::CopyToClipboard(_))));
    state.conversation.chat_selection = None;
    assert_eq!(buffer, render_offline(&mut state, 24, 12));
}

#[tokio::test]
async fn bubble_expansion_flushes_feedback_before_rendering_the_body() {
    use ylva::agent::UiUpdate;
    let history = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (mut backend, last_buffer, _) = InspectableTestBackend::new(80, 24);
    backend.frame_history = Some(Arc::clone(&history));
    let mut terminal = Terminal::new(backend).expect("test terminal");
    let (action_tx, action_rx) = mpsc::channel(64);
    let (update_tx, update_rx) = mpsc::channel(64);
    let engine = Arc::new(EngineImpl::new(
        Arc::new(MockProvider::new(Vec::new(), Duration::ZERO)),
        common::test_engine_config(),
    ));
    let (agent, _agent_updates) = AgentLoop::new(engine, Arc::new(ToolRegistry::new()));
    let run_handle = tokio::spawn(async move {
        tui_driver::run_tui(
            &mut terminal,
            agent,
            update_rx,
            None,
            action_rx,
            &mut NoopClipboard,
        )
        .await
        .expect("run_tui completed")
    });
    update_tx.send(UiUpdate::StreamStarted).await.unwrap();
    update_tx
        .send(UiUpdate::AppendThinking("Expansion body marker".into()))
        .await
        .unwrap();
    update_tx.send(UiUpdate::StreamDone).await.unwrap();
    for (title, expanded) in [("▼ Thought", false), ("▶ Thought", true)] {
        let buffer =
            wait_for_frame(&last_buffer, |buffer| rendered_text(buffer).contains(title)).await;
        let title_row = rendered_text(&buffer)
            .lines()
            .position(|line| line.contains(title))
            .unwrap() as u16;
        if expanded {
            history.lock().expect("frame history lock").clear();
        }
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            send(
                &action_tx,
                AppAction::MouseInput(MouseEvent {
                    kind,
                    column: 5,
                    row: title_row,
                    modifiers: KeyModifiers::NONE,
                }),
            )
            .await;
        }
    }
    wait_for_frame(&last_buffer, |buffer| {
        rendered_text(buffer).contains("Expansion body marker")
    })
    .await;
    drop(action_tx);
    run_handle.await.expect("run task joined");
    let frames: Vec<_> = history
        .lock()
        .expect("frame history lock")
        .iter()
        .map(rendered_text)
        .collect();
    let feedback = frames
        .iter()
        .position(|text| text.contains("Expanding…"))
        .expect("feedback flushed");
    let expanded = frames
        .iter()
        .position(|text| text.contains("Expansion body marker"))
        .expect("body flushed");
    assert!(feedback < expanded);
    assert!(!frames[feedback].contains("Expansion body marker"));
    assert!(!frames[expanded].contains("Expanding…"));
}
