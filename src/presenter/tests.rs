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

use std::time::Instant;

use crossterm::event::{KeyCode, KeyModifiers, MouseEventKind};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::Terminal;
use uuid::Uuid;

use super::*;
use crate::tui_reducer_state::{
    ChildRunInfo, ConfirmKey, ContextUsage, DomainEvent, Effect, UserMessageState,
};

mod domain;
mod keyboard;
mod mouse;
mod tool_bubbles;
use crate::tui_reducer_state::{
    AppState, ChatEntry, ChatSelection, ContextUsageSource, ConversationView, Focus, InputEvent,
    ModelChoice, ResultSection, SlashOverlay, ToolCallState, TuiMode,
};
use crate::ProviderModelOption;

fn empty_textarea() -> TextareaContext {
    TextareaContext {
        is_empty: true,
        text: String::new(),
        cursor_row: 0,
        cursor_row_is_last: true,
    }
}

fn textarea_with(text: &str) -> TextareaContext {
    TextareaContext {
        is_empty: text.trim().is_empty(),
        text: text.to_string(),
        cursor_row: 0,
        cursor_row_is_last: true,
    }
}

#[test]
fn keyboard_and_mouse_approval_verdicts_preserve_effect_order() {
    for approve in [true, false] {
        let tool_id = Uuid::new_v4();
        for use_mouse in [true, false] {
            let mut state = AppState::new();
            update(
                &mut state,
                TuiEvent::Domain(DomainEvent::ToolPending {
                    id: tool_id,
                    name: "write".into(),
                    arguments: serde_json::json!({"path": "file.txt"}),
                    content_sections: vec![],
                    title_arguments: vec![],
                    run_id: None,
                }),
                &empty_textarea(),
            );
            state.layout.approve_button = ratatui::layout::Rect::new(0, 10, 5, 1);
            state.layout.deny_button = ratatui::layout::Rect::new(6, 10, 5, 1);
            let event = if use_mouse {
                TuiEvent::Mouse(crossterm::event::MouseEvent {
                    kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
                    column: if approve { 1 } else { 7 },
                    row: 10,
                    modifiers: KeyModifiers::NONE,
                })
            } else if approve {
                TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Enter)))
            } else {
                TuiEvent::Input(InputEvent::Escape)
            };
            let expected_verdict = if approve {
                Effect::ApproveTool { id: tool_id }
            } else {
                Effect::DenyTool {
                    id: tool_id,
                    reason: String::new(),
                }
            };

            assert_eq!(
                update(&mut state, event, &empty_textarea()),
                [expected_verdict, Effect::RestoreInputState]
            );
            assert_eq!(state.focus, Focus::MainInput);
        }
    }
}

fn render_app_state_buffer(state: &mut AppState) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");
    let mut textarea = ratatui_textarea::TextArea::default();
    terminal
        .draw(|frame| {
            crate::render::render(
                frame,
                state,
                &mut textarea,
                None,
                None,
                &crate::ToolRegistry::new(),
            );
        })
        .expect("render test frame");
    terminal.backend().buffer().clone()
}

fn prime_domain_logging() {
    update(
        &mut AppState::new(),
        TuiEvent::Domain(DomainEvent::AgentIdle),
        &empty_textarea(),
    );
    // Other presenter tests can first-register this callsite without a
    // subscriber. Refresh only after that registration has completed.
    tracing::callsite::rebuild_interest_cache();
}

fn key_event(code: KeyCode) -> crossterm::event::KeyEvent {
    crossterm::event::KeyEvent::new(code, KeyModifiers::NONE)
}

// ── Phase 6.10 drill-in view tests ─────────────────────────────────────────

fn tracked_child(state: &mut AppState) -> Uuid {
    let run_id = Uuid::new_v4();
    state.start_child_run(crate::tui_reducer_state::ChildRunInfo {
        run_id,
        bubble_id: Uuid::new_v4(),
        agent_type: "reviewer".to_string(),
        task: "look into flaky test".to_string(),
        label: "Investigate flaky test".to_string(),
    });
    run_id
}

// ── Ctrl+C tests ──────────────────────────────────────────────────────────

// ── Escape tests ──────────────────────────────────────────────────────────

// ── ShowHelp ──────────────────────────────────────────────────────────────

// ── StopSignal (external SIGINT/SIGTERM) ──────────────────────────────────

// ── Enter / submit ────────────────────────────────────────────────────────

// ── Help scroll ───────────────────────────────────────────────────────────

// ── Mouse scroll ──────────────────────────────────────────────────────────

/// Sets up `state` with a single `spawn_subagent` `ToolBubble` at
/// `bubble_id`, plus the title-row/rendered-line bookkeeping needed for a
/// click at row 0 (the bubble's title row — there is no top-padding row
/// before it) to resolve to it, and returns a `MouseEvent::Up` click at
/// that row.
fn spawn_bubble_click_fixture(
    state: &mut AppState,
    bubble_id: Uuid,
) -> crossterm::event::MouseEvent {
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
    // (start_row=0, end_row=2, bubble_id) — title row is start.
    state.conversation.bubble_title_rows = vec![(0, 2, bubble_id)];
    state.conversation.chat_drag_active = true;
    state.conversation.chat_rendered_lines = vec![String::new(), String::new(), String::new()];
    // A zero-width selection at the title row is a plain click, not a drag.
    state.conversation.chat_selection = Some(ChatSelection {
        anchor_line: 0,
        anchor_col: 3,
        cursor_line: 0,
        cursor_col: 3,
    });
    crossterm::event::MouseEvent {
        kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
        column: 3,
        row: 0,
        modifiers: KeyModifiers::NONE,
    }
}

// ── Domain events ─────────────────────────────────────────────────────────

#[test]
fn steer_queued_mid_round_commits_after_intervening_thinking_and_tool_bubbles() {
    // Regression test: a steer typed while thinking/tool-calls are still
    // in flight for the current round must NOT be committed to
    // `chat_entries` at `Enter`-press time (that would fix its position
    // before the round's content is flushed, wedging it ahead of
    // chronologically-earlier bubbles). It must land in `chat_entries`
    // only once `SteerApplied` fires, after everything already flushed.
    let mut state = AppState::new();
    state.mode = TuiMode::Streaming;

    // Thinking starts, then a tool call is issued (auto-flushes the
    // thinking bubble), then the user types a steer mid-round.
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::StreamThinkingChunk("deciding".to_string())),
        &empty_textarea(),
    );
    let tool_id_1 = Uuid::new_v4();
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolStart {
            id: tool_id_1,
            name: "read".to_string(),
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
            run_id: None,
        }),
        &empty_textarea(),
    );
    state.queue_steer_message("steer me".to_string());

    // The steer must be staged, not committed, while the round is still
    // in flight — even though a ThinkingMessage and ToolBubble already
    // exist ahead of it in `chat_entries`.
    assert_eq!(
        state.queued_steer_texts,
        vec!["steer me".to_string()],
        "steer should be staged, not yet committed"
    );
    assert!(
        !state
            .chat_entries
            .iter()
            .any(|entry| matches!(entry, ChatEntry::UserMessage { .. })),
        "chat_entries must not contain the steer yet: {:?}",
        state.chat_entries
    );

    // A second tool call runs and completes while the steer is still
    // staged.
    let tool_id_2 = Uuid::new_v4();
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::ToolStart {
            id: tool_id_2,
            name: "write".to_string(),
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
            run_id: None,
        }),
        &empty_textarea(),
    );

    // Now the steer gets applied to the model (backend injected it and
    // is starting the next request).
    update(
        &mut state,
        TuiEvent::Domain(DomainEvent::SteerApplied { run_id: None }),
        &empty_textarea(),
    );

    assert!(
        state.queued_steer_texts.is_empty(),
        "staged steer should be drained on SteerApplied"
    );
    assert_eq!(state.chat_entries.len(), 4, "{:?}", state.chat_entries);
    assert!(matches!(
        &state.chat_entries[0],
        ChatEntry::ThinkingMessage { text, .. } if text == "deciding"
    ));
    assert!(matches!(
        &state.chat_entries[1],
        ChatEntry::ToolBubble { id, .. } if *id == tool_id_1
    ));
    assert!(matches!(
        &state.chat_entries[2],
        ChatEntry::ToolBubble { id, .. } if *id == tool_id_2
    ));
    assert!(
        matches!(
            &state.chat_entries[3],
            ChatEntry::UserMessage { text, state: UserMessageState::Steered } if text == "steer me"
        ),
        "steer bubble must land AFTER the intervening thinking/tool bubbles: {:?}",
        state.chat_entries[3]
    );
}
