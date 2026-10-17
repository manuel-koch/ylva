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
use crate::tui_reducer_state::{ToolCallState, UserMessageState};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::Terminal;
use std::sync::Arc;
use std::time::Duration;

fn render_offline(state: &mut AppState, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let mut textarea = TextArea::default();
    terminal
        .draw(|frame| {
            render(
                frame,
                state,
                &mut textarea,
                None,
                None,
                &ToolRegistry::new(),
            )
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

fn completed_render_fixture(content: String) -> ChatEntry {
    ChatEntry::ToolBubble {
        id: uuid::Uuid::new_v4(),
        name: "read".into(),
        collapsed: false,
        state: Box::new(ToolCallState::Done {
            arguments: serde_json::json!({}),
            title_arguments: vec![(
                "path".into(),
                serde_json::json!("資料/a long wrapped file name with Unicode.rs"),
            )],
            content_sections: vec![],
            result_section: crate::tui_reducer_state::ResultSection::plain(content),
            summary: vec![],
            duration: Duration::from_secs(1),
        }),
    }
}

fn empty_textarea_context() -> crate::TextareaContext {
    crate::TextareaContext {
        is_empty: true,
        text: String::new(),
        cursor_row: 0,
        cursor_row_is_last: true,
    }
}

fn present_render_event(state: &mut AppState, event: crate::DomainEvent) {
    crate::presenter::update(
        state,
        crate::TuiEvent::Domain(event),
        &empty_textarea_context(),
    );
}

fn running_subagent_fixture(state: &mut AppState, collapsed: bool) -> (uuid::Uuid, uuid::Uuid) {
    let bubble_id = uuid::Uuid::new_v4();
    let run_id = uuid::Uuid::new_v4();
    present_render_event(
        state,
        crate::DomainEvent::ToolStart {
            id: bubble_id,
            name: "spawn_subagent".into(),
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
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
    present_render_event(
        state,
        crate::DomainEvent::SubagentStarted {
            run_id,
            call_id: bubble_id,
            agent_type: "default".into(),
            task: "Inspect".into(),
            label: "Inspect".into(),
        },
    );
    (bubble_id, run_id)
}

#[test]
fn live_thinking_subsecond_redraws_reuse_chrome_and_body() {
    for subagent_view in [false, true] {
        for collapsed in [false, true] {
            let mut state = AppState::new();
            state.enable_render_cache();
            let run_id = if subagent_view {
                let (_, run_id) = running_subagent_fixture(&mut state, collapsed);
                present_render_event(
                    &mut state,
                    crate::DomainEvent::SubagentToken {
                        run_id,
                        text: "reasoning".into(),
                        is_thinking: true,
                    },
                );
                state.switch_to_subagent_view(run_id);
                Some(run_id)
            } else {
                present_render_event(&mut state, crate::DomainEvent::StreamStarted);
                present_render_event(
                    &mut state,
                    crate::DomainEvent::StreamThinkingChunk("reasoning".into()),
                );
                None
            };
            let mut previous_layouts = Vec::new();
            for elapsed_millis in [9_100, 9_600, 10_100] {
                let turn_buffer = if let Some(run_id) = run_id {
                    &mut state
                        .subagent_conversations
                        .get_mut(&run_id)
                        .unwrap()
                        .turn_buffer
                } else {
                    &mut state.turn_buffer
                };
                turn_buffer.partial_thinking_collapsed = collapsed;
                turn_buffer.thinking_started_at =
                    Some(std::time::Instant::now() - Duration::from_millis(elapsed_millis));
                render_offline(&mut state, 120, 40);
                if elapsed_millis == 9_600 {
                    let current_layouts = &state.render_cache.conversation.layouts;
                    assert_eq!(previous_layouts.len(), current_layouts.len());
                    assert!(previous_layouts
                        .iter()
                        .zip(current_layouts)
                        .all(|(previous, current)| Arc::ptr_eq(previous, current)));
                }
                previous_layouts = state.render_cache.conversation.layouts.clone();
            }
        }
    }
}

#[test]
fn running_subagent_subsecond_redraws_reuse_chrome_and_body() {
    for subagent_view in [false, true] {
        for collapsed in [false, true] {
            let mut state = AppState::new();
            state.enable_render_cache();
            let (bubble_id, run_id) = running_subagent_fixture(&mut state, collapsed);
            if subagent_view {
                state.switch_to_subagent_view(run_id);
            }
            for elapsed_seconds in [12, 22] {
                state
                    .subagent_conversations
                    .get_mut(&run_id)
                    .unwrap()
                    .started_at = Some(
                    std::time::Instant::now() - Duration::from_millis(elapsed_seconds * 1000 + 100),
                );
                present_render_event(
                    &mut state,
                    crate::DomainEvent::ToolProgress {
                        id: bubble_id,
                        title_arguments: vec![("status".into(), serde_json::json!("running"))],
                    },
                );
                render_offline(&mut state, 80, 20);
                let previous_layouts = if subagent_view {
                    state.render_cache.header.layouts.clone()
                } else {
                    state.render_cache.conversation.layouts.clone()
                };
                state
                    .subagent_conversations
                    .get_mut(&run_id)
                    .unwrap()
                    .started_at = Some(
                    std::time::Instant::now() - Duration::from_millis(elapsed_seconds * 1000 + 600),
                );
                render_offline(&mut state, 80, 20);
                let current_layouts = if subagent_view {
                    &state.render_cache.header.layouts
                } else {
                    &state.render_cache.conversation.layouts
                };
                assert_eq!(previous_layouts.len(), current_layouts.len());
                assert!(previous_layouts
                    .iter()
                    .zip(current_layouts)
                    .all(|(previous, current)| Arc::ptr_eq(previous, current)));
            }
        }
    }
}

#[test]
fn large_completed_result_warm_render_reuses_selection_storage() {
    use crate::tui_reducer_state::ResultSection;
    let source_body = (0..600)
        .map(|index| {
            format!("fn highlighted_fixture_{index}() {{ println!(\"日本語 {index}\"); }}\n")
        })
        .collect::<String>();
    let mut state = AppState::new();
    state.enable_render_cache();
    state.chat_entries.push(ChatEntry::ToolBubble {
        id: uuid::Uuid::new_v4(),
        name: "read".into(),
        state: Box::new(ToolCallState::Done {
            arguments: serde_json::json!({}),
            content_sections: vec![crate::agent::tools::Section::Content(
                crate::agent::tools::ContentSection {
                    label: Some("highlighting_fixture.rs".into()),
                    content: source_body,
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
    let first_buffer = render_offline(&mut state, 120, 40);
    let selection_storage = state.conversation.chat_rendered_lines.as_ptr();
    let body_builds = state.render_cache.body_layout_builds;
    let second_buffer = render_offline(&mut state, 120, 40);
    assert_eq!(
        selection_storage,
        state.conversation.chat_rendered_lines.as_ptr()
    );
    assert_eq!(body_builds, state.render_cache.body_layout_builds);
    assert_eq!(first_buffer, second_buffer);
    assert!(state.render_cache.painted_rows <= 40);
    assert!(state
        .conversation
        .chat_rendered_lines
        .iter()
        .any(|line| line.contains("fn highlighted_fixture_599")));
    assert!(state.conversation.chat_rendered_lines.len() > 500);
}

#[test]
fn subagent_header_live_chrome_and_background_updates_reuse_large_bodies() {
    use crate::DomainEvent;
    let mut state = AppState::new();
    state.enable_render_cache();
    let bubble_id = uuid::Uuid::new_v4();
    let run_id = uuid::Uuid::new_v4();
    let sections = vec![crate::agent::tools::Section::Content(
        crate::agent::tools::ContentSection {
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
        DomainEvent::SubagentStarted {
            run_id,
            call_id: bubble_id,
            agent_type: "reviewer".into(),
            task: "Inspect".into(),
            label: "Inspect".into(),
        },
    );
    present_render_event(
        &mut state,
        DomainEvent::SubagentToken {
            run_id,
            text: "subagent transcript\n\n".repeat(3_000),
            is_thinking: false,
        },
    );
    present_render_event(
        &mut state,
        DomainEvent::SubagentDone {
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
    state.switch_to_subagent_view(run_id);
    render_offline(&mut state, 120, 40);
    let header_body = Arc::clone(&state.render_cache.header.layouts[1]);
    let subagent_body = Arc::clone(&state.render_cache.conversation.layouts[1]);
    let builds = state.render_cache.body_layout_builds;
    assert!(state.render_cache.header.height > 3_000);
    assert_eq!(state.layout.header_bubble.height, 16);
    state.spinner_frame += 1;
    state.subagent_view.header_bubble_scroll_offset = 1_500;
    render_offline(&mut state, 120, 40);
    assert_eq!(builds, state.render_cache.body_layout_builds);
    assert!(Arc::ptr_eq(
        &header_body,
        &state.render_cache.header.layouts[1]
    ));
    assert!(Arc::ptr_eq(
        &subagent_body,
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
        .subagent_view
        .header_rendered_lines
        .iter()
        .any(|line| line.contains("details=")));
    let second_run = uuid::Uuid::new_v4();
    present_render_event(
        &mut state,
        DomainEvent::SubagentStarted {
            run_id: second_run,
            call_id: uuid::Uuid::new_v4(),
            agent_type: "reviewer".into(),
            task: "other".into(),
            label: "other".into(),
        },
    );
    present_render_event(
        &mut state,
        DomainEvent::SubagentToken {
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
    assert!(state.subagent_view.header_rendered_lines.is_empty());
    render_offline(&mut state, 120, 40);
    assert!(state.render_cache.header.layouts.is_empty());
    assert!(state.pending_scroll_to_bubble.is_none());
}

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
            let mut state = AppState::new();
            state.enable_render_cache();
            state.chat_entries = entries.clone();
            for width in [50, 120] {
                for offset in [0, 1_500, usize::MAX] {
                    state.conversation.scroll_locked = false;
                    state.conversation.scroll_offset = offset;
                    let actual = render_offline(&mut state, width, 40);
                    let mut legacy = TuiState::new();
                    legacy.chat_entries = entries.clone();
                    legacy.conversation.scroll_locked = false;
                    legacy.conversation.scroll_offset = offset;
                    let mut terminal = Terminal::new(TestBackend::new(width, 40)).unwrap();
                    let mut textarea = ratatui_textarea::TextArea::default();
                    terminal
                        .draw(|frame| {
                            crate::tui_widgets::draw(
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
    use crate::DomainEvent;
    let mut state = AppState::new();
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
fn same_length_tool_replacement_collapse_retention_and_public_compatibility() {
    let mut state = AppState::new();
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
    let ToolCallState::Done { result_section, .. } = bubble.as_mut() else {
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
    let mut direct = AppState::new();
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
    let mut state = AppState::new();
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
    state.conversation.chat_selection = Some(crate::ChatSelection {
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
    let effects = crate::presenter::update(
        &mut state,
        crate::TuiEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 10,
            row: 0,
            modifiers: KeyModifiers::NONE,
        }),
        &empty_textarea_context(),
    );
    assert!(effects
        .iter()
        .any(|effect| matches!(effect, crate::Effect::CopyToClipboard(_))));
    state.conversation.chat_selection = None;
    assert_eq!(buffer, render_offline(&mut state, 24, 12));
}
