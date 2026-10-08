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
                        state.conversation.chat_rendered_lines,
                        legacy.conversation.chat_rendered_lines
                    );
                    assert_eq!(
                        state.conversation.chat_rendered_line_prefix_widths,
                        legacy.conversation.chat_rendered_line_prefix_widths
                    );
                    assert_eq!(
                        state.conversation.bubble_title_rows,
                        legacy.conversation.bubble_title_rows
                    );
                    assert_eq!(
                        state.conversation.scroll_offset,
                        legacy.conversation.scroll_offset
                    );
                    let builds = state.render_cache.body_layout_builds;
                    let _ = render_offline(&mut state, width, 40);
                    assert_eq!(builds, state.render_cache.body_layout_builds);
                    assert!(state.render_cache.painted_rows <= 40);
                }
            }
        }
    }
}

#[test]
fn stream_updates_append_and_compaction_preserve_or_invalidate_the_correct_slots() {
    use ylva::DomainEvent;
    let mut state = ylva::AppState::new();
    state.enable_render_cache();
    state
        .chat_entries
        .push(completed_render_fixture("completed\n\n".repeat(3_000)));
    present_render_event(&mut state, DomainEvent::StreamStarted);
    present_render_event(&mut state, DomainEvent::StreamChunk("first".into()));
    render_offline(&mut state, 120, 40);
    let body_builds = state.render_cache.body_layout_builds;
    present_render_event(&mut state, DomainEvent::StreamChunk(" second".into()));
    render_offline(&mut state, 120, 40);
    assert_eq!(state.render_cache.body_layout_builds, body_builds + 1);
    let completed = Arc::clone(&state.render_cache.conversation.layouts[2]);
    state.spinner_frame += 1;
    state.conversation.scroll_locked = false;
    state.conversation.scroll_offset = 10;
    render_offline(&mut state, 120, 30);
    assert!(Arc::ptr_eq(
        &completed,
        &state.render_cache.conversation.layouts[2]
    ));
    present_render_event(
        &mut state,
        DomainEvent::StreamThinkingChunk("reasoning".into()),
    );
    render_offline(&mut state, 120, 40);
    assert!(Arc::ptr_eq(
        &completed,
        &state.render_cache.conversation.layouts[2]
    ));
    present_render_event(&mut state, DomainEvent::StreamDone);
    state.push_notice("notice".into());
    state.queue_steer_message("queued".into());
    render_offline(&mut state, 120, 40);
    assert!(Arc::ptr_eq(
        &completed,
        &state.render_cache.conversation.layouts[2]
    ));
    state.commit_queued_steer_messages(UserMessageState::Cancelled);
    render_offline(&mut state, 120, 40);
    assert!(Arc::ptr_eq(
        &completed,
        &state.render_cache.conversation.layouts[2]
    ));
    present_render_event(
        &mut state,
        DomainEvent::CompactionApplied {
            run_id: None,
            entry_index: 0,
            summarized_messages: 1,
        },
    );
    render_offline(&mut state, 120, 40);
    assert!(matches!(
        state.chat_entries[0],
        ChatEntry::CompactionMarker { .. }
    ));
    assert!(state.conversation.chat_rendered_lines[1].contains("compaction"));
    assert!(!Arc::ptr_eq(
        &completed,
        &state.render_cache.conversation.layouts[3]
    ));
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
    let buffer = render_offline(&mut state, 120, 40);
    let text: String = buffer.content().iter().map(Cell::symbol).collect();
    assert!(text.contains("Thinking…"));
    assert!(text.contains("1 message queued"));
    present_render_event(&mut state, ylva::DomainEvent::StreamChunk("answer".into()));
    let buffer = render_offline(&mut state, 120, 40);
    let text: String = buffer.content().iter().map(Cell::symbol).collect();
    assert!(text.contains("Streaming…"));
    assert!(text.contains("1 message queued"));
}

#[test]
fn same_length_tool_replacement_collapse_retention_and_public_compatibility() {
    let mut state = ylva::AppState::new();
    state.enable_render_cache();
    state
        .chat_entries
        .push(completed_render_fixture("old".into()));
    let ChatEntry::ToolBubble { id, .. } = state.chat_entries[0] else {
        unreachable!()
    };
    render_offline(&mut state, 80, 40);
    let original = Arc::clone(&state.render_cache.conversation.layouts[2]);
    state.toggle_tool_bubble_collapsed(id);
    render_offline(&mut state, 80, 40);
    assert!(state.render_cache.retained_bytes() > 0);
    state.toggle_tool_bubble_collapsed(id);
    render_offline(&mut state, 80, 40);
    assert!(Arc::ptr_eq(
        &original,
        &state.render_cache.conversation.layouts[2]
    ));
    let Some(ChatEntry::ToolBubble { state: bubble, .. }) = state.find_tool_bubble_mut(id) else {
        unreachable!()
    };
    let ylva::ToolCallState::Done { result_section, .. } = bubble.as_mut() else {
        unreachable!()
    };
    result_section.content = "new".into();
    render_offline(&mut state, 80, 40);
    assert!(state
        .conversation
        .chat_rendered_lines
        .iter()
        .any(|line| line == "new"));
    assert!(!Arc::ptr_eq(
        &original,
        &state.render_cache.conversation.layouts[2]
    ));
    let mut direct = ylva::AppState::new();
    direct.chat_entries = vec![ChatEntry::Notice { text: "old".into() }];
    render_offline(&mut direct, 80, 40);
    direct.chat_entries[0] = ChatEntry::Notice { text: "new".into() };
    render_offline(&mut direct, 80, 40);
    assert!(direct
        .conversation
        .chat_rendered_lines
        .iter()
        .any(|line| line.contains("new")));
    state.clear_conversation();
    assert!(state.render_cache.conversation.layouts.is_empty());
    assert!(state.conversation.chat_rendered_lines.is_empty());
    assert_eq!(state.render_cache.retained_bytes(), 0);
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
    assert_eq!(
        state.conversation.scroll_offset,
        state.render_cache.conversation.height - state.layout.chat_area.rect.height as usize
    );
    assert!(buffer.content().iter().any(|cell| cell.symbol() == "9"));
    let visible = state.render_cache.conversation.visible_rows(
        state.conversation.scroll_offset,
        state.layout.chat_area.rect.height as usize,
    );
    assert!(visible
        .iter()
        .flat_map(|line| &line.spans)
        .any(|span| span.content.contains("34999")));
    let builds = state.render_cache.body_layout_builds;
    state.conversation.chat_selection = Some(ylva::ChatSelection {
        anchor_line: 1,
        anchor_col: 5,
        cursor_line: state.render_cache.conversation.height - 1,
        cursor_col: 10,
    });
    render_offline(&mut state, 24, 12);
    assert_eq!(builds, state.render_cache.body_layout_builds);
    assert!(state.render_cache.painted_rows <= 12);
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
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
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
        .send(UiUpdate::AppendThinking(
            "Expansion body marker".to_string(),
        ))
        .await
        .unwrap();
    update_tx.send(UiUpdate::StreamDone).await.unwrap();
    let title_row = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let text = rendered_text(&last_buffer.lock().expect("buffer snapshot lock"));
            if let Some(row) = text.lines().position(|line| line.contains("▼ Thought")) {
                break row as u16;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("thinking title must render");
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        action_tx
            .send(AppAction::MouseInput(MouseEvent {
                kind,
                column: 5,
                row: title_row,
                modifiers: KeyModifiers::NONE,
            }))
            .await
            .unwrap();
    }
    let title_row = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let text = rendered_text(&last_buffer.lock().expect("buffer snapshot lock"));
            if let Some(row) = text.lines().position(|line| line.contains("▶ Thought")) {
                break row as u16;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("collapsed thinking title must render");
    history.lock().expect("frame history lock").clear();
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        action_tx
            .send(AppAction::MouseInput(MouseEvent {
                kind,
                column: 5,
                row: title_row,
                modifiers: KeyModifiers::NONE,
            }))
            .await
            .unwrap();
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if rendered_text(&last_buffer.lock().expect("buffer snapshot lock"))
                .contains("Expansion body marker")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("expanded body must render");
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
        .expect("expansion feedback must be flushed");
    let expanded = frames
        .iter()
        .position(|text| text.contains("Expansion body marker"))
        .expect("expanded body must be flushed");
    assert!(feedback < expanded);
    assert!(!frames[feedback].contains("Expansion body marker"));
    assert!(!frames[expanded].contains("Expanding…"));
}
