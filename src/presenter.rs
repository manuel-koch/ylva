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

//! Pure Presenter layer for the reducer pattern.
//!
//! [`update`] is the single entry point: it takes the current [`AppState`] by
//! mutable reference (so pure state mutations — scroll offsets, focus changes,
//! mode transitions — can happen in-place) and returns a `Vec<Effect>` for
//! every I/O action the Driver must execute.
//!
//! **No `use ratatui`**, no `use ratatui_textarea`**, no channel sends.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyModifiers, MouseEventKind};
use uuid::Uuid;

use crate::app_state::{
    AppState, ChatEntry, ChatSelection, DomainEvent, Effect, Focus, HistoryDirection, InputEvent,
    ToolCallState, TuiMode, UserMessageState,
};

// ─── Public surface ───────────────────────────────────────────────────────────

/// Typed event surface accepted by [`update`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiEvent {
    Input(InputEvent),
    Mouse(crossterm::event::MouseEvent),
    Domain(DomainEvent),
}

/// Read-only snapshot of the textarea state that the Presenter needs for
/// routing decisions without importing ratatui-textarea.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextareaContext {
    /// `true` when the textarea contains only whitespace / is empty.
    pub is_empty: bool,
    /// Joined text of all textarea lines (used for submit / dedent).
    pub text: String,
    /// Zero-based cursor row inside the textarea.
    pub cursor_row: usize,
    /// `true` when the cursor is on the last row of the textarea.
    pub cursor_row_is_last: bool,
}

/// Apply one event to `state`, return the effects the Driver must execute.
pub fn update(state: &mut AppState, event: TuiEvent, textarea: &TextareaContext) -> Vec<Effect> {
    match event {
        TuiEvent::Input(input_event) => route_keyboard(state, input_event, textarea),
        TuiEvent::Mouse(mouse_event) => route_mouse(state, mouse_event, textarea),
        TuiEvent::Domain(domain_event) => route_domain(state, domain_event),
    }
}

// ─── Keyboard routing ─────────────────────────────────────────────────────────

fn route_keyboard(
    state: &mut AppState,
    event: InputEvent,
    textarea: &TextareaContext,
) -> Vec<Effect> {
    match &state.focus {
        Focus::HelpPopup => keyboard_help(state, event),
        Focus::ToolApproval { .. } => keyboard_approval(state, event, textarea),
        Focus::MainInput => keyboard_main(state, event, textarea),
    }
}

fn keyboard_help(state: &mut AppState, event: InputEvent) -> Vec<Effect> {
    match event {
        InputEvent::ShowHelp | InputEvent::Escape => {
            state.focus = Focus::MainInput;
            state.help_scroll_offset = 0;
            state.help_scroll_x = 0;
            vec![]
        }
        InputEvent::SigInt => vec![Effect::StopAgent],
        InputEvent::CtrlC => handle_ctrl_c_generic(state, true),
        InputEvent::KeyInput(key_event) => {
            match key_event.code {
                KeyCode::Up => {
                    state.help_scroll_up(1);
                }
                KeyCode::Down => {
                    state.help_scroll_down(1);
                }
                KeyCode::Left => {
                    state.help_scroll_left(4);
                }
                KeyCode::Right => {
                    state.help_scroll_right(4);
                }
                KeyCode::PageUp => {
                    state.help_scroll_up(3);
                }
                KeyCode::PageDown => {
                    state.help_scroll_down(3);
                }
                _ => {}
            }
            vec![]
        }
        InputEvent::Paste(_) => vec![],
    }
}

fn keyboard_approval(
    state: &mut AppState,
    event: InputEvent,
    textarea: &TextareaContext,
) -> Vec<Effect> {
    let tool_id = match &state.focus {
        Focus::ToolApproval { tool_id, .. } => *tool_id,
        _ => return vec![],
    };
    match event {
        InputEvent::CtrlC | InputEvent::Escape => {
            state.focus = Focus::MainInput;
            vec![Effect::DenyTool { id: tool_id }, Effect::RestoreTextarea]
        }
        InputEvent::SigInt => vec![Effect::StopAgent],
        InputEvent::KeyInput(key_event) if key_event.code == KeyCode::Enter => {
            state.focus = Focus::MainInput;
            if textarea.is_empty {
                vec![Effect::ApproveTool { id: tool_id }, Effect::RestoreTextarea]
            } else {
                vec![Effect::DenyTool { id: tool_id }, Effect::RestoreTextarea]
            }
        }
        InputEvent::KeyInput(key_event) => vec![Effect::TextareaKey(key_event)],
        InputEvent::ShowHelp => {
            state.focus = Focus::HelpPopup;
            vec![]
        }
        InputEvent::Paste(text) => vec![Effect::Paste(text)],
    }
}

fn keyboard_main(
    state: &mut AppState,
    event: InputEvent,
    textarea: &TextareaContext,
) -> Vec<Effect> {
    match event {
        InputEvent::ShowHelp => {
            state.focus = Focus::HelpPopup;
            vec![]
        }
        InputEvent::SigInt => vec![Effect::StopAgent],
        InputEvent::CtrlC => handle_ctrl_c_generic(state, textarea.is_empty),
        InputEvent::Escape => handle_escape_main(state, textarea),
        InputEvent::Paste(text) => vec![Effect::Paste(text)],
        InputEvent::KeyInput(key_event) => handle_key_main(state, key_event, textarea),
    }
}

/// Ctrl+C logic for non-approval contexts.
fn handle_ctrl_c_generic(state: &mut AppState, textarea_is_empty: bool) -> Vec<Effect> {
    let mode_is_idle = matches!(state.mode, TuiMode::Idle);

    // Busy with non-empty input: clear the textarea first.
    if !mode_is_idle && !textarea_is_empty {
        let effects = vec![Effect::ClearTextarea];
        // Reset double-press state by restoring mode if in double-press.
        if let TuiMode::DoublePressAbort { return_to, .. } = std::mem::replace(
            &mut state.mode,
            TuiMode::Idle, // placeholder, overwritten below
        ) {
            state.mode = *return_to;
        }
        return effects;
    }

    let current_mode = std::mem::replace(&mut state.mode, TuiMode::Idle);

    match current_mode {
        TuiMode::DoublePressAbort { return_to, .. } => {
            if matches!(*return_to, TuiMode::Idle) {
                // Idle double-press → quit.
                state.mode = TuiMode::Idle;
                vec![Effect::Quit]
            } else {
                // Busy double-press → stop agent and exit TUI on done.
                tracing::info!("double Ctrl-C: entering Aborting, stop requested");
                state.mode = TuiMode::Aborting;
                state.aborting_exit = true;
                vec![Effect::StopAgent]
            }
        }
        mode => {
            // First press: enter double-press abort mode.
            state.mode = TuiMode::DoublePressAbort {
                pressed_at: Instant::now(),
                return_to: Box::new(mode),
            };
            vec![]
        }
    }
}

fn handle_escape_main(state: &mut AppState, textarea: &TextareaContext) -> Vec<Effect> {
    let mode_is_idle = matches!(state.mode, TuiMode::Idle);

    if mode_is_idle {
        // Idle: clear textarea and reset history.
        state.history_index = None;
        state.draft = String::new();
        return vec![Effect::ClearTextarea];
    }

    // Busy with non-empty input: clear first.
    if !textarea.is_empty {
        state.history_index = None;
        state.draft = String::new();
        // Reset double-press state.
        if let TuiMode::DoublePressAbort { return_to, .. } =
            std::mem::replace(&mut state.mode, TuiMode::Idle)
        {
            state.mode = *return_to;
        }
        return vec![Effect::ClearTextarea];
    }

    // Busy with empty input: double-press → stop, but stay in the TUI.
    // Unlike Ctrl+C, Esc does not set aborting_exit; the TUI remains open
    // when StreamCancelled transitions mode to Idle.
    let current_mode = std::mem::replace(&mut state.mode, TuiMode::Idle);
    match current_mode {
        TuiMode::DoublePressAbort { return_to, .. } => {
            // Enter Aborting for visual feedback ("Aborting…" in footer +
            // spinner) but leave aborting_exit = false so the Driver stays open.
            state.mode = TuiMode::Aborting;
            let _ = return_to;
            vec![Effect::StopAgent]
        }
        mode => {
            state.mode = TuiMode::DoublePressAbort {
                pressed_at: Instant::now(),
                return_to: Box::new(mode),
            };
            vec![]
        }
    }
}

fn handle_key_main(
    state: &mut AppState,
    key_event: crossterm::event::KeyEvent,
    textarea: &TextareaContext,
) -> Vec<Effect> {
    match key_event.code {
        // Shift+Enter or Alt+Enter: insert a newline, exit history browsing.
        KeyCode::Enter
            if key_event.modifiers.contains(KeyModifiers::SHIFT)
                || key_event.modifiers.contains(KeyModifiers::ALT) =>
        {
            vec![Effect::ExitBrowsing, Effect::TextareaKey(key_event)]
        }

        KeyCode::Enter => {
            let raw_text = &textarea.text;
            if raw_text.trim().is_empty() {
                return vec![];
            }
            let text = dedent(raw_text);
            let mode_is_idle = matches!(state.mode, TuiMode::Idle);
            let bubble_state = if mode_is_idle {
                UserMessageState::Sent
            } else {
                UserMessageState::Queued
            };
            state.push_user_bubble(text.clone(), bubble_state);
            if mode_is_idle {
                state.mode = TuiMode::Pending;
            }
            vec![
                Effect::SubmitPrompt {
                    steer: !mode_is_idle,
                },
                Effect::ClearTextarea,
            ]
        }

        KeyCode::PageUp => {
            state.scroll_up(3);
            vec![]
        }

        KeyCode::PageDown => {
            state.scroll_down(3);
            vec![]
        }

        KeyCode::Home | KeyCode::End => vec![Effect::TextareaKey(key_event)],

        KeyCode::Up => {
            if textarea.cursor_row == 0 {
                vec![Effect::LoadHistory {
                    direction: HistoryDirection::Up,
                }]
            } else {
                vec![Effect::TextareaKey(key_event)]
            }
        }

        KeyCode::Down => {
            if textarea.cursor_row_is_last {
                vec![Effect::LoadHistory {
                    direction: HistoryDirection::Down,
                }]
            } else {
                vec![Effect::TextareaKey(key_event)]
            }
        }

        _ => {
            // Any edit key while browsing history exits browsing.
            vec![Effect::ExitBrowsing, Effect::TextareaKey(key_event)]
        }
    }
}

// ─── Mouse routing ────────────────────────────────────────────────────────────

fn route_mouse(
    state: &mut AppState,
    event: crossterm::event::MouseEvent,
    _textarea: &TextareaContext,
) -> Vec<Effect> {
    let column = event.column;
    let row = event.row;

    match event.kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let scroll_up = matches!(event.kind, MouseEventKind::ScrollUp);
            if matches!(state.focus, Focus::HelpPopup) {
                if scroll_up {
                    state.help_scroll_up(1);
                } else {
                    state.help_scroll_down(1);
                }
            } else if state.layout.input_area.contains(column, row) {
                // Input area scroll is driver-owned (textarea); emit a key effect.
                // Use a synthetic ScrollUp/Down via a placeholder effect.
                // The driver handles input scroll via PlaceCursorAtMousePos or
                // direct textarea.scroll calls. We use a raw mouse effect pass-through.
                // Since we can't embed mouse events in Effect, we emit nothing
                // and rely on the driver's fallback scroll handler for input.
                // (Input area scroll is a driver concern; see tui_driver.rs.)
            } else {
                if scroll_up {
                    state.scroll_up(1);
                } else {
                    state.scroll_down(1);
                }
            }
            vec![]
        }

        MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
            // Close modal on click outside.
            if matches!(state.focus, Focus::HelpPopup) {
                if !state.layout.help_popup.hit(column, row) {
                    state.focus = Focus::MainInput;
                    state.help_scroll_offset = 0;
                    state.help_scroll_x = 0;
                }
                return vec![];
            }

            // Approve / Deny button clicks while in tool-approval focus.
            if let Focus::ToolApproval { tool_id, .. } = state.focus {
                let approve = state.layout.approve_button;
                let deny = state.layout.deny_button;
                let in_approve = column >= approve.x
                    && column < approve.x + approve.width
                    && row >= approve.y
                    && row < approve.y + approve.height;
                let in_deny = column >= deny.x
                    && column < deny.x + deny.width
                    && row >= deny.y
                    && row < deny.y + deny.height;
                if in_approve {
                    state.focus = Focus::MainInput;
                    return vec![Effect::ApproveTool { id: tool_id }, Effect::RestoreTextarea];
                }
                if in_deny {
                    state.focus = Focus::MainInput;
                    return vec![Effect::DenyTool { id: tool_id }, Effect::RestoreTextarea];
                }
            }

            let in_chat = state.layout.chat_area.contains(column, row);
            let in_input = state.layout.input_area.contains(column, row);

            if in_chat {
                let anchor_line = state
                    .scroll_offset
                    .saturating_add((row.saturating_sub(state.layout.chat_area.rect.y)) as usize);
                let anchor_col = (column.saturating_sub(state.layout.chat_area.rect.x)) as usize;
                state.chat_selection = Some(ChatSelection {
                    anchor_line,
                    anchor_col,
                    cursor_line: anchor_line,
                    cursor_col: anchor_col,
                });
                state.chat_drag_active = true;
                vec![]
            } else if in_input {
                vec![Effect::PlaceCursorAtMousePos { column, row }]
            } else {
                vec![]
            }
        }

        MouseEventKind::Drag(crossterm::event::MouseButton::Left) => {
            if state.chat_drag_active {
                if let Some(sel) = state.chat_selection.as_mut() {
                    sel.cursor_line = state.scroll_offset.saturating_add(
                        (row.saturating_sub(state.layout.chat_area.rect.y)) as usize,
                    );
                    sel.cursor_col =
                        (column.saturating_sub(state.layout.chat_area.rect.x)) as usize;
                }
                vec![]
            } else {
                vec![Effect::ExtendInputSelection { column, row }]
            }
        }

        MouseEventKind::Up(crossterm::event::MouseButton::Left) => {
            if state.chat_drag_active {
                state.chat_drag_active = false;
                let selected_text = state.extract_chat_selection_text();
                if let Some(text) = selected_text {
                    if !text.is_empty() {
                        return vec![Effect::CopyToClipboard(text)];
                    }
                }
                // Plain click — toggle bubble if clicked on title row.
                if let Some(sel) = state.chat_selection {
                    let clicked_line = sel.anchor_line;
                    let clicked_col = sel.anchor_col;
                    if let Some((_, _, bubble_id)) = state
                        .bubble_title_rows
                        .iter()
                        .find(|(start, end, _)| {
                            clicked_line == start + 1
                                || (clicked_col == 0
                                    && clicked_line >= *start
                                    && clicked_line <= *end)
                        })
                        .copied()
                    {
                        state.toggle_tool_bubble_collapsed(bubble_id);
                    }
                }
                state.chat_selection = None;
                vec![]
            } else {
                vec![Effect::FinalizeInputSelection]
            }
        }

        _ => vec![],
    }
}

// ─── Domain routing ───────────────────────────────────────────────────────────

fn route_domain(state: &mut AppState, event: DomainEvent) -> Vec<Effect> {
    match state.mode.clone() {
        TuiMode::Idle => domain_idle(state, event),
        TuiMode::Pending => domain_pending(state, event),
        TuiMode::Streaming => domain_streaming(state, event),
        TuiMode::Aborting => domain_aborting(state, event),
        TuiMode::DoublePressAbort {
            pressed_at,
            return_to,
        } => domain_double_press(state, event, pressed_at, return_to),
    }
}

fn domain_idle(state: &mut AppState, event: DomainEvent) -> Vec<Effect> {
    match event {
        DomainEvent::StreamStarted => {
            state.flush_partial_assistant_message(false);
            state.mode = TuiMode::Streaming;
            state.partial_assistant_text.clear();
            vec![]
        }
        DomainEvent::ToolPending {
            id,
            name,
            arguments,
            content_sections,
            title_arguments,
        } => {
            state.flush_partial_assistant_message(false);
            state.chat_entries.push(ChatEntry::ToolBubble {
                id,
                name: name.clone(),
                state: ToolCallState::Pending {
                    arguments: arguments.clone(),
                    content_sections: content_sections.clone(),
                    title_arguments: title_arguments.clone(),
                },
                collapsed: true,
            });
            state.focus = Focus::ToolApproval {
                tool_id: id,
                tool_name: name,
            };
            vec![Effect::SaveTextareaAndClear]
        }
        DomainEvent::Error(message) => {
            state.chat_entries.push(ChatEntry::ErrorLine { message });
            state.mode = TuiMode::Idle;
            vec![]
        }
        DomainEvent::SteerQueued => {
            state.steer_indicator = true;
            vec![]
        }
        DomainEvent::SteerApplied => {
            state.transition_queued_messages_to(UserMessageState::Steered);
            state.steer_indicator = false;
            vec![]
        }
        DomainEvent::AgentIdle => {
            state.mode = TuiMode::Idle;
            state.focus = Focus::MainInput;
            vec![]
        }
        DomainEvent::ToolStart {
            id,
            name,
            arguments,
            content_sections,
            title_arguments,
        } => {
            update_tool_bubble_to_running(
                state,
                id,
                name,
                arguments,
                content_sections,
                title_arguments,
            );
            vec![]
        }
        DomainEvent::ToolDone {
            id,
            name,
            result,
            summary,
            duration,
            content_sections,
        } => {
            update_tool_bubble_to_done(
                state,
                id,
                name,
                result,
                content_sections,
                summary,
                duration,
            );
            vec![]
        }
        DomainEvent::ToolKilled { id } => {
            update_tool_bubble_to_killed(state, id);
            vec![]
        }
        DomainEvent::ToolDenied { id } => {
            update_tool_bubble_to_denied(state, id);
            if matches!(state.focus, Focus::ToolApproval { tool_id, .. } if tool_id == id) {
                state.focus = Focus::MainInput;
            }
            vec![]
        }
        DomainEvent::ConfirmExpired => vec![],
        _ => vec![],
    }
}

fn domain_pending(state: &mut AppState, event: DomainEvent) -> Vec<Effect> {
    match event {
        DomainEvent::StreamStarted => {
            state.flush_partial_assistant_message(false);
            state.mode = TuiMode::Streaming;
            state.partial_assistant_text.clear();
            vec![]
        }
        DomainEvent::StreamDone => {
            state.flush_partial_assistant_message(false);
            state.mode = TuiMode::Idle;
            state.scroll_locked = true;
            state.scroll_offset = 0;
            vec![]
        }
        DomainEvent::StreamCancelled => {
            state.flush_partial_assistant_message(true);
            state.transition_queued_messages_to(UserMessageState::Cancelled);
            state.steer_indicator = false;
            state.mode = TuiMode::Idle;
            state.scroll_locked = true;
            state.scroll_offset = 0;
            vec![]
        }
        DomainEvent::Error(message) => {
            state.chat_entries.push(ChatEntry::ErrorLine { message });
            state.mode = TuiMode::Idle;
            vec![]
        }
        DomainEvent::ToolPending {
            id,
            name,
            arguments,
            content_sections,
            title_arguments,
        } => {
            state.flush_partial_assistant_message(false);
            state.chat_entries.push(ChatEntry::ToolBubble {
                id,
                name: name.clone(),
                state: ToolCallState::Pending {
                    arguments: arguments.clone(),
                    content_sections: content_sections.clone(),
                    title_arguments: title_arguments.clone(),
                },
                collapsed: true,
            });
            state.focus = Focus::ToolApproval {
                tool_id: id,
                tool_name: name,
            };
            vec![Effect::SaveTextareaAndClear]
        }
        DomainEvent::SteerQueued => {
            state.steer_indicator = true;
            vec![]
        }
        DomainEvent::SteerApplied => {
            state.transition_queued_messages_to(UserMessageState::Steered);
            state.steer_indicator = false;
            vec![]
        }
        DomainEvent::AgentIdle => {
            state.mode = TuiMode::Idle;
            state.focus = Focus::MainInput;
            vec![]
        }
        DomainEvent::ToolStart {
            id,
            name,
            arguments,
            content_sections,
            title_arguments,
        } => {
            update_tool_bubble_to_running(
                state,
                id,
                name,
                arguments,
                content_sections,
                title_arguments,
            );
            vec![]
        }
        DomainEvent::ToolDone {
            id,
            name,
            result,
            summary,
            duration,
            content_sections,
        } => {
            update_tool_bubble_to_done(
                state,
                id,
                name,
                result,
                content_sections,
                summary,
                duration,
            );
            vec![]
        }
        DomainEvent::ToolKilled { id } => {
            update_tool_bubble_to_killed(state, id);
            vec![]
        }
        DomainEvent::ToolDenied { id } => {
            update_tool_bubble_to_denied(state, id);
            if matches!(state.focus, Focus::ToolApproval { tool_id, .. } if tool_id == id) {
                state.focus = Focus::MainInput;
            }
            vec![]
        }
        DomainEvent::ConfirmExpired => vec![],
        _ => vec![],
    }
}

fn domain_streaming(state: &mut AppState, event: DomainEvent) -> Vec<Effect> {
    match event {
        DomainEvent::StreamChunk(text) => {
            state.partial_assistant_text.push_str(&text);
            if state.scroll_locked {
                state.scroll_offset = 0;
            }
            vec![]
        }
        DomainEvent::StreamDone => {
            state.flush_partial_assistant_message(false);
            state.mode = TuiMode::Idle;
            state.scroll_locked = true;
            state.scroll_offset = 0;
            vec![]
        }
        DomainEvent::StreamCancelled => {
            state.flush_partial_assistant_message(true);
            state.transition_queued_messages_to(UserMessageState::Cancelled);
            state.steer_indicator = false;
            if matches!(state.focus, Focus::ToolApproval { .. }) {
                state.focus = Focus::MainInput;
            }
            state.mode = TuiMode::Idle;
            state.scroll_locked = true;
            state.scroll_offset = 0;
            vec![]
        }
        DomainEvent::ToolPending {
            id,
            name,
            arguments,
            content_sections,
            title_arguments,
        } => {
            state.flush_partial_assistant_message(false);
            state.chat_entries.push(ChatEntry::ToolBubble {
                id,
                name: name.clone(),
                state: ToolCallState::Pending {
                    arguments: arguments.clone(),
                    content_sections: content_sections.clone(),
                    title_arguments: title_arguments.clone(),
                },
                collapsed: true,
            });
            state.focus = Focus::ToolApproval {
                tool_id: id,
                tool_name: name,
            };
            vec![Effect::SaveTextareaAndClear]
        }
        DomainEvent::ToolStart {
            id,
            name,
            arguments,
            content_sections,
            title_arguments,
        } => {
            update_tool_bubble_to_running(
                state,
                id,
                name,
                arguments,
                content_sections,
                title_arguments,
            );
            vec![]
        }
        DomainEvent::ToolDone {
            id,
            name,
            result,
            summary,
            duration,
            content_sections,
        } => {
            update_tool_bubble_to_done(
                state,
                id,
                name,
                result,
                content_sections,
                summary,
                duration,
            );
            vec![]
        }
        DomainEvent::ToolKilled { id } => {
            update_tool_bubble_to_killed(state, id);
            vec![]
        }
        DomainEvent::ToolDenied { id } => {
            update_tool_bubble_to_denied(state, id);
            if matches!(state.focus, Focus::ToolApproval { tool_id, .. } if tool_id == id) {
                state.focus = Focus::MainInput;
            }
            vec![]
        }
        DomainEvent::Error(message) => {
            state.chat_entries.push(ChatEntry::ErrorLine { message });
            state.mode = TuiMode::Idle;
            vec![]
        }
        DomainEvent::SteerQueued => {
            state.steer_indicator = true;
            vec![]
        }
        DomainEvent::SteerApplied => {
            state.transition_queued_messages_to(UserMessageState::Steered);
            state.steer_indicator = false;
            vec![]
        }
        DomainEvent::AgentIdle => {
            state.mode = TuiMode::Idle;
            state.focus = Focus::MainInput;
            vec![]
        }
        DomainEvent::ConfirmExpired => vec![],
        _ => vec![],
    }
}

fn domain_aborting(state: &mut AppState, event: DomainEvent) -> Vec<Effect> {
    tracing::debug!(event = ?std::mem::discriminant(&event), "domain_aborting: received event");
    match event {
        DomainEvent::StreamCancelled => {
            tracing::info!(
                aborting_exit = state.aborting_exit,
                "aborting: stream cancelled → idle"
            );
            state.flush_partial_assistant_message(true);
            state.transition_queued_messages_to(UserMessageState::Cancelled);
            state.steer_indicator = false;
            if matches!(state.focus, Focus::ToolApproval { .. }) {
                state.focus = Focus::MainInput;
            }
            state.mode = TuiMode::Idle;
            state.scroll_locked = true;
            state.scroll_offset = 0;
            if std::mem::replace(&mut state.aborting_exit, false) {
                vec![Effect::Quit]
            } else {
                vec![]
            }
        }
        DomainEvent::AgentIdle => {
            state.mode = TuiMode::Idle;
            state.focus = Focus::MainInput;
            if std::mem::replace(&mut state.aborting_exit, false) {
                vec![Effect::Quit]
            } else {
                vec![]
            }
        }
        DomainEvent::ToolPending { id, .. } => {
            // While aborting, deny immediately without entering approval sub-mode.
            vec![Effect::DenyTool { id }]
        }
        DomainEvent::ToolStart {
            id,
            name,
            arguments,
            content_sections,
            title_arguments,
        } => {
            update_tool_bubble_to_running(
                state,
                id,
                name,
                arguments,
                content_sections,
                title_arguments,
            );
            vec![]
        }
        DomainEvent::ToolDone {
            id,
            name,
            result,
            summary,
            duration,
            content_sections,
        } => {
            update_tool_bubble_to_done(
                state,
                id,
                name,
                result,
                content_sections,
                summary,
                duration,
            );
            vec![]
        }
        DomainEvent::ToolKilled { id } => {
            tracing::info!(%id, aborting_exit = state.aborting_exit, "aborting: tool killed → idle");
            update_tool_bubble_to_killed(state, id);
            state.mode = TuiMode::Idle;
            state.focus = Focus::MainInput;
            if std::mem::replace(&mut state.aborting_exit, false) {
                vec![Effect::Quit]
            } else {
                vec![]
            }
        }
        DomainEvent::ToolDenied { id } => {
            update_tool_bubble_to_denied(state, id);
            vec![]
        }
        DomainEvent::Error(message) => {
            state.chat_entries.push(ChatEntry::ErrorLine { message });
            state.mode = TuiMode::Idle;
            vec![]
        }
        DomainEvent::StreamDone => {
            state.flush_partial_assistant_message(false);
            state.mode = TuiMode::Idle;
            state.scroll_locked = true;
            state.scroll_offset = 0;
            vec![]
        }
        DomainEvent::StreamChunk(text) => {
            state.partial_assistant_text.push_str(&text);
            vec![]
        }
        DomainEvent::ConfirmExpired => vec![],
        _ => vec![],
    }
}

fn domain_double_press(
    state: &mut AppState,
    event: DomainEvent,
    pressed_at: Instant,
    return_to: Box<TuiMode>,
) -> Vec<Effect> {
    match event {
        DomainEvent::ConfirmExpired => {
            // Confirm window expired — restore the underlying mode.
            state.mode = *return_to;
            vec![]
        }
        other_event => {
            // Temporarily restore the underlying mode so domain handlers can
            // process the event. If the handler didn't change the mode (i.e.
            // it was a side-effect-only update like StreamChunk or ToolDone),
            // reinstate DoublePressAbort so the confirm window remains active.
            // If the handler did change mode (e.g. StreamDone → Idle), the
            // new mode stays.
            state.mode = *return_to.clone();
            let effects = route_domain(state, other_event);
            if state.mode == *return_to {
                state.mode = TuiMode::DoublePressAbort {
                    pressed_at,
                    return_to,
                };
            }
            effects
        }
    }
}

// ─── Tool bubble update helpers ───────────────────────────────────────────────

fn update_tool_bubble_to_running(
    state: &mut AppState,
    id: Uuid,
    name: String,
    arguments: serde_json::Value,
    content_sections: Vec<crate::agent::tools::ContentSection>,
    title_arguments: Vec<(String, serde_json::Value)>,
) {
    if let Some(entry) = state.find_tool_bubble_mut(id) {
        if let ChatEntry::ToolBubble {
            state: bubble_state,
            ..
        } = entry
        {
            *bubble_state = ToolCallState::Running {
                arguments: arguments.clone(),
                content_sections: content_sections.clone(),
                title_arguments: title_arguments.clone(),
            };
        }
    } else {
        state.chat_entries.push(ChatEntry::ToolBubble {
            id,
            name,
            state: ToolCallState::Running {
                arguments,
                content_sections,
                title_arguments,
            },
            collapsed: true,
        });
    }
    // Clear ToolApproval focus if it was for this tool.
    if matches!(state.focus, Focus::ToolApproval { tool_id, .. } if tool_id == id) {
        state.focus = Focus::MainInput;
    }
}

fn update_tool_bubble_to_done(
    state: &mut AppState,
    id: Uuid,
    name: String,
    result: String,
    content_sections: Vec<crate::agent::tools::ContentSection>,
    summary: Vec<(String, String)>,
    duration: std::time::Duration,
) {
    let (previous_arguments, previous_title_arguments) = state
        .find_tool_bubble_mut(id)
        .and_then(|entry| {
            if let ChatEntry::ToolBubble {
                state: bubble_state,
                ..
            } = entry
            {
                match bubble_state {
                    ToolCallState::Running {
                        arguments,
                        title_arguments,
                        ..
                    }
                    | ToolCallState::Pending {
                        arguments,
                        title_arguments,
                        ..
                    } => Some((arguments.clone(), title_arguments.clone())),
                    _ => None,
                }
            } else {
                None
            }
        })
        .unwrap_or((serde_json::Value::Null, vec![]));
    if let Some(entry) = state.find_tool_bubble_mut(id) {
        if let ChatEntry::ToolBubble {
            state: bubble_state,
            ..
        } = entry
        {
            *bubble_state = ToolCallState::Done {
                arguments: previous_arguments,
                result,
                content_sections,
                summary,
                duration,
                title_arguments: previous_title_arguments,
            };
        }
    } else {
        state.chat_entries.push(ChatEntry::ToolBubble {
            id,
            name,
            state: ToolCallState::Done {
                arguments: serde_json::Value::Null,
                result,
                content_sections,
                summary,
                duration,
                title_arguments: vec![],
            },
            collapsed: true,
        });
    }
}

fn update_tool_bubble_to_killed(state: &mut AppState, id: Uuid) {
    if let Some(entry) = state.find_tool_bubble_mut(id) {
        if let ChatEntry::ToolBubble {
            state: bubble_state,
            ..
        } = entry
        {
            *bubble_state = ToolCallState::Killed;
        }
    } else {
        state.chat_entries.push(ChatEntry::ToolBubble {
            id,
            name: String::new(),
            state: ToolCallState::Killed,
            collapsed: true,
        });
    }
    if matches!(state.focus, Focus::ToolApproval { tool_id, .. } if tool_id == id) {
        state.focus = Focus::MainInput;
    }
}

fn update_tool_bubble_to_denied(state: &mut AppState, id: Uuid) {
    if let Some(ChatEntry::ToolBubble {
        state: bubble_state,
        ..
    }) = state.find_tool_bubble_mut(id)
    {
        let (preserved_arguments, preserved_content_sections, preserved_title_arguments) =
            match bubble_state {
                ToolCallState::Pending {
                    arguments,
                    content_sections,
                    title_arguments,
                }
                | ToolCallState::Running {
                    arguments,
                    content_sections,
                    title_arguments,
                } => (
                    arguments.clone(),
                    content_sections.clone(),
                    title_arguments.clone(),
                ),
                _ => (serde_json::Value::Null, vec![], vec![]),
            };
        *bubble_state = ToolCallState::Denied {
            arguments: preserved_arguments,
            error: "Denied by user".to_string(),
            content_sections: preserved_content_sections,
            title_arguments: preserved_title_arguments,
        };
    }
}

// ─── Dedent helper ────────────────────────────────────────────────────────────

/// Remove common leading whitespace and strip leading/trailing blank lines.
fn dedent(text: &str) -> String {
    let dedented = textwrap::dedent(text);
    let lines: Vec<&str> = dedented.lines().collect();
    let Some(start) = lines.iter().position(|line| !line.trim().is_empty()) else {
        return String::new();
    };
    let end = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .map(|index| index + 1)
        .unwrap_or(lines.len());
    lines[start..end].join("\n")
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use crossterm::event::{KeyCode, KeyModifiers};
    use uuid::Uuid;

    use super::*;
    use crate::app_state::{AppState, ChatEntry, Focus, InputEvent, ToolCallState, TuiMode};

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

    fn key_event(code: KeyCode) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(code, KeyModifiers::NONE)
    }

    // ── Ctrl+C tests ──────────────────────────────────────────────────────────

    #[test]
    fn ctrl_c_while_idle_first_press_sets_double_press_abort_mode() {
        let mut state = AppState::new();
        state.mode = TuiMode::Idle;
        let effects = update(
            &mut state,
            TuiEvent::Input(InputEvent::CtrlC),
            &empty_textarea(),
        );
        assert!(
            matches!(state.mode, TuiMode::DoublePressAbort { ref return_to, .. } if matches!(**return_to, TuiMode::Idle)),
            "expected DoublePressAbort{{return_to: Idle}}, got {:?}",
            state.mode
        );
        assert!(!effects.contains(&Effect::Quit));
    }

    #[test]
    fn ctrl_c_while_idle_second_press_returns_quit_effect() {
        let mut state = AppState::new();
        state.mode = TuiMode::DoublePressAbort {
            pressed_at: Instant::now(),
            return_to: Box::new(TuiMode::Idle),
        };
        let effects = update(
            &mut state,
            TuiEvent::Input(InputEvent::CtrlC),
            &empty_textarea(),
        );
        assert!(effects.contains(&Effect::Quit));
    }

    #[test]
    fn ctrl_c_while_busy_first_press_records_confirm() {
        let mut state = AppState::new();
        state.mode = TuiMode::Streaming;
        let effects = update(
            &mut state,
            TuiEvent::Input(InputEvent::CtrlC),
            &empty_textarea(),
        );
        assert!(
            matches!(state.mode, TuiMode::DoublePressAbort { ref return_to, .. } if matches!(**return_to, TuiMode::Streaming)),
            "expected DoublePressAbort{{return_to: Streaming}}"
        );
        assert!(!effects.contains(&Effect::StopAgent));
    }

    #[test]
    fn ctrl_c_while_busy_second_press_returns_stop_agent_effect() {
        let mut state = AppState::new();
        state.mode = TuiMode::DoublePressAbort {
            pressed_at: Instant::now(),
            return_to: Box::new(TuiMode::Streaming),
        };
        let effects = update(
            &mut state,
            TuiEvent::Input(InputEvent::CtrlC),
            &empty_textarea(),
        );
        assert!(effects.contains(&Effect::StopAgent));
        assert_eq!(state.mode, TuiMode::Aborting);
        assert!(
            state.aborting_exit,
            "aborting_exit must be set after double-press Ctrl-C while busy"
        );
    }

    #[test]
    fn double_ctrl_c_while_streaming_quits_after_stream_cancelled() {
        let mut state = AppState::new();
        // First Ctrl-C: enters DoublePressAbort.
        state.mode = TuiMode::Streaming;
        update(
            &mut state,
            TuiEvent::Input(InputEvent::CtrlC),
            &empty_textarea(),
        );
        // Second Ctrl-C: sends StopAgent and sets aborting_exit.
        let effects = update(
            &mut state,
            TuiEvent::Input(InputEvent::CtrlC),
            &empty_textarea(),
        );
        assert!(effects.contains(&Effect::StopAgent));
        assert!(state.aborting_exit);
        // StreamCancelled arrives: must emit Quit.
        let effects = update(
            &mut state,
            TuiEvent::Domain(DomainEvent::StreamCancelled),
            &empty_textarea(),
        );
        assert!(
            effects.contains(&Effect::Quit),
            "StreamCancelled after double Ctrl-C while streaming must emit Quit"
        );
    }

    #[test]
    fn double_ctrl_c_while_tool_running_quits_after_tool_killed() {
        let mut state = AppState::new();
        state.mode = TuiMode::Streaming;
        // First Ctrl-C.
        update(
            &mut state,
            TuiEvent::Input(InputEvent::CtrlC),
            &empty_textarea(),
        );
        // Second Ctrl-C: stop agent, set aborting_exit.
        let effects = update(
            &mut state,
            TuiEvent::Input(InputEvent::CtrlC),
            &empty_textarea(),
        );
        assert!(effects.contains(&Effect::StopAgent));
        assert!(state.aborting_exit);
        // ToolKilled arrives (no StreamCancelled because no HTTP stream was active).
        let effects = update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolKilled { id: Uuid::new_v4() }),
            &empty_textarea(),
        );
        assert!(
            effects.contains(&Effect::Quit),
            "ToolKilled after double Ctrl-C while tool running must emit Quit"
        );
        assert_eq!(state.mode, TuiMode::Idle, "mode must return to Idle");
    }

    #[test]
    fn ctrl_c_while_tool_approval_denies_tool() {
        let mut state = AppState::new();
        let tool_id = Uuid::new_v4();
        state.focus = Focus::ToolApproval {
            tool_id,
            tool_name: "test_tool".to_string(),
        };
        let effects = update(
            &mut state,
            TuiEvent::Input(InputEvent::CtrlC),
            &empty_textarea(),
        );
        assert!(effects.contains(&Effect::DenyTool { id: tool_id }));
        assert!(effects.contains(&Effect::RestoreTextarea));
        assert_eq!(state.focus, Focus::MainInput);
    }

    // ── Escape tests ──────────────────────────────────────────────────────────

    #[test]
    fn escape_while_help_open_closes_help() {
        let mut state = AppState::new();
        state.focus = Focus::HelpPopup;
        update(
            &mut state,
            TuiEvent::Input(InputEvent::Escape),
            &empty_textarea(),
        );
        assert_eq!(state.focus, Focus::MainInput);
    }

    #[test]
    fn escape_while_tool_approval_denies_tool() {
        let mut state = AppState::new();
        let tool_id = Uuid::new_v4();
        state.focus = Focus::ToolApproval {
            tool_id,
            tool_name: "test_tool".to_string(),
        };
        let effects = update(
            &mut state,
            TuiEvent::Input(InputEvent::Escape),
            &empty_textarea(),
        );
        assert!(effects.contains(&Effect::DenyTool { id: tool_id }));
        assert!(effects.contains(&Effect::RestoreTextarea));
    }

    #[test]
    fn escape_while_busy_empty_input_triggers_confirm() {
        let mut state = AppState::new();
        state.mode = TuiMode::Streaming;
        update(
            &mut state,
            TuiEvent::Input(InputEvent::Escape),
            &empty_textarea(),
        );
        assert!(
            matches!(state.mode, TuiMode::DoublePressAbort { .. }),
            "expected DoublePressAbort after first Escape"
        );
        // Second press → stop agent but stay in TUI (mode should be Aborting
        // for visual feedback, but aborting_exit must stay false).
        let effects = update(
            &mut state,
            TuiEvent::Input(InputEvent::Escape),
            &empty_textarea(),
        );
        assert!(effects.contains(&Effect::StopAgent));
        assert!(
            matches!(state.mode, TuiMode::Aborting),
            "Esc second press must set Aborting for visual feedback"
        );
        assert!(
            !state.aborting_exit,
            "Esc second press must not set aborting_exit — TUI must stay open"
        );
    }

    // ── ShowHelp ──────────────────────────────────────────────────────────────

    #[test]
    fn show_help_toggles_focus_to_help_popup() {
        let mut state = AppState::new();
        state.focus = Focus::MainInput;
        update(
            &mut state,
            TuiEvent::Input(InputEvent::ShowHelp),
            &empty_textarea(),
        );
        assert_eq!(state.focus, Focus::HelpPopup);
        update(
            &mut state,
            TuiEvent::Input(InputEvent::ShowHelp),
            &empty_textarea(),
        );
        assert_eq!(state.focus, Focus::MainInput);
    }

    // ── SigInt ────────────────────────────────────────────────────────────────

    #[test]
    fn sigint_sends_stop_agent_effect_immediately() {
        let mut state = AppState::new();
        let effects = update(
            &mut state,
            TuiEvent::Input(InputEvent::SigInt),
            &empty_textarea(),
        );
        assert!(effects.contains(&Effect::StopAgent));
    }

    // ── Enter / submit ────────────────────────────────────────────────────────

    #[test]
    fn enter_with_text_returns_submit_prompt_effect() {
        let mut state = AppState::new();
        let effects = update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Enter))),
            &textarea_with("hello"),
        );
        assert!(effects.contains(&Effect::SubmitPrompt { steer: false }));
        assert!(effects.contains(&Effect::ClearTextarea));
    }

    #[test]
    fn enter_while_approval_empty_returns_approve_tool_effect() {
        let mut state = AppState::new();
        let tool_id = Uuid::new_v4();
        state.focus = Focus::ToolApproval {
            tool_id,
            tool_name: "test_tool".to_string(),
        };
        let effects = update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Enter))),
            &empty_textarea(),
        );
        assert!(effects.contains(&Effect::ApproveTool { id: tool_id }));
        assert!(effects.contains(&Effect::RestoreTextarea));
    }

    #[test]
    fn enter_while_approval_with_text_returns_deny_tool_effect() {
        let mut state = AppState::new();
        let tool_id = Uuid::new_v4();
        state.focus = Focus::ToolApproval {
            tool_id,
            tool_name: "test_tool".to_string(),
        };
        let effects = update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Enter))),
            &textarea_with("deny reason"),
        );
        assert!(effects.contains(&Effect::DenyTool { id: tool_id }));
        assert!(effects.contains(&Effect::RestoreTextarea));
    }

    // ── Help scroll ───────────────────────────────────────────────────────────

    #[test]
    fn keyboard_in_help_mode_scrolls_help() {
        let mut state = AppState::new();
        state.focus = Focus::HelpPopup;
        state.help_scroll_offset = 5;
        update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Down))),
            &empty_textarea(),
        );
        assert_eq!(state.help_scroll_offset, 6);
    }

    // ── Mouse scroll ──────────────────────────────────────────────────────────

    #[test]
    fn mouse_scroll_up_in_chat_area_decrements_scroll_offset() {
        let mut state = AppState::new();
        state.scroll_offset = 5;
        state.scroll_locked = false;
        let mouse = crossterm::event::MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 10,
            row: 10,
            modifiers: KeyModifiers::NONE,
        };
        update(&mut state, TuiEvent::Mouse(mouse), &empty_textarea());
        assert_eq!(state.scroll_offset, 4);
    }

    // ── Mouse click closes modal ───────────────────────────────────────────────

    #[test]
    fn mouse_click_outside_help_popup_closes_modal() {
        let mut state = AppState::new();
        state.focus = Focus::HelpPopup;
        // layout.help_popup.rect is default (0,0,0,0), so any click is "outside".
        let mouse = crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 50,
            row: 20,
            modifiers: KeyModifiers::NONE,
        };
        update(&mut state, TuiEvent::Mouse(mouse), &empty_textarea());
        assert_eq!(state.focus, Focus::MainInput);
    }

    #[test]
    fn chat_drag_mouse_up_emits_copy_to_clipboard_effect() {
        // The Presenter must emit CopyToClipboard so the Driver can handle the
        // I/O without the test touching the OS clipboard.
        let mut state = AppState::new();
        state.chat_drag_active = true;
        state.chat_rendered_lines = vec!["hello world".to_string()];
        state.chat_selection = Some(ChatSelection {
            anchor_line: 0,
            anchor_col: 0,
            cursor_line: 0,
            cursor_col: 5,
        });
        let mouse = crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
            column: 5,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        let effects = update(&mut state, TuiEvent::Mouse(mouse), &empty_textarea());
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::CopyToClipboard(t) if t == "hello")),
            "expected CopyToClipboard(\"hello\"), got {effects:?}"
        );
        assert!(!state.chat_drag_active, "drag should be cleared");
    }

    // ── Domain events ─────────────────────────────────────────────────────────

    #[test]
    fn domain_stream_done_transitions_mode_to_idle() {
        let mut state = AppState::new();
        state.mode = TuiMode::Streaming;
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::StreamDone),
            &empty_textarea(),
        );
        assert_eq!(state.mode, TuiMode::Idle);
    }

    #[test]
    fn domain_tool_pending_while_aborting_returns_deny_effect() {
        let mut state = AppState::new();
        state.mode = TuiMode::Aborting;
        let tool_id = Uuid::new_v4();
        let effects = update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolPending {
                id: tool_id,
                name: "test_tool".to_string(),
                arguments: serde_json::json!({}),
                content_sections: vec![],

                title_arguments: vec![],
            }),
            &empty_textarea(),
        );
        assert!(effects.contains(&Effect::DenyTool { id: tool_id }));
        // Focus must not change to ToolApproval.
        assert_ne!(
            state.focus,
            Focus::ToolApproval {
                tool_id,
                tool_name: "test_tool".to_string()
            }
        );
    }

    #[test]
    fn confirm_expired_event_clears_double_press_abort_mode() {
        let mut state = AppState::new();
        state.mode = TuiMode::DoublePressAbort {
            pressed_at: Instant::now(),
            return_to: Box::new(TuiMode::Streaming),
        };
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ConfirmExpired),
            &empty_textarea(),
        );
        assert_eq!(state.mode, TuiMode::Streaming);
    }

    #[test]
    fn stream_chunk_during_double_press_abort_preserves_confirm_window() {
        // A StreamChunk must not destroy the DoublePressAbort mode — the user
        // must still be able to land the second Esc while the model streams.
        let mut state = AppState::new();
        let pressed_at = Instant::now();
        state.mode = TuiMode::DoublePressAbort {
            pressed_at,
            return_to: Box::new(TuiMode::Streaming),
        };
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::StreamChunk("hello".to_string())),
            &empty_textarea(),
        );
        assert!(
            matches!(state.mode, TuiMode::DoublePressAbort { .. }),
            "DoublePressAbort must survive a StreamChunk"
        );
        assert_eq!(
            state.partial_assistant_text, "hello",
            "chunk must still be appended"
        );
    }

    #[test]
    fn denied_tool_bubble_retains_arguments() {
        let mut state = AppState::new();
        state.mode = TuiMode::Idle;
        let tool_id = Uuid::new_v4();
        let args = serde_json::json!({"path": "/tmp/foo", "content": "bar"});
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolPending {
                id: tool_id,
                name: "write_file".to_string(),
                arguments: args.clone(),
                content_sections: vec![],

                title_arguments: vec![],
            }),
            &empty_textarea(),
        );
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolDenied { id: tool_id }),
            &empty_textarea(),
        );
        let denied_bubble = state.chat_entries.iter().find_map(|entry| {
            if let ChatEntry::ToolBubble {
                state: ToolCallState::Denied { arguments, .. },
                ..
            } = entry
            {
                return Some(arguments.clone());
            }
            None
        });
        assert_eq!(
            denied_bubble.expect("denied bubble should exist"),
            args,
            "Denied bubble must retain the arguments from the Pending state"
        );
    }
}
