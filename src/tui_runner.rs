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

//! TUI event loop and the production terminal entry point.
//!
//! The event loop is a pure consumer of `UiUpdate` and a sender of
//! `AgentCommand`; `AgentLoop` itself is untouched. All context-dependent input
//! handling (double-press confirm, approval sub-mode) lives here.

use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::style::Style;
use ratatui::Terminal;
use ratatui_textarea::TextArea;
use tokio::select;
use tokio::sync::mpsc;

use crate::agent::{AgentCommand, AgentLoop, AgentMode, UiUpdate};
use crate::app::{ChatSelection, HistoryDirection, TuiState, UserMessageState};
use crate::events::{spawn_crossterm_bridge, AppAction};
use crate::llm::Provider;
use crate::ui::{self, ButtonAreas};
use crate::ToolRegistry;

/// Window (seconds) for the key-agnostic double-press confirm on `Ctrl+C`/`Esc`.
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(2);

/// Interval between spinner frame advances while the agent is busy.
const SPINNER_TICK_INTERVAL: Duration = Duration::from_millis(150);

/// Capacity of the bounded action channel between the crossterm bridge and the
/// async loop, and the per-frame `UiUpdate` drain cap.
const CHANNEL_CAPACITY: usize = 64;

/// Outcome of handling one [`AppAction`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionOutcome {
    Continue,
    Quit,
}

/// Apply the standard Ylva textarea configuration: word-or-glyph wrapping and
/// no cursor-line underline decoration.
///
/// Call this whenever a new `TextArea` is created or reset so that all
/// textarea instances have consistent styling.
fn configure_textarea(textarea: &mut TextArea<'_>) {
    textarea.set_wrap_mode(ratatui_textarea::WrapMode::WordOrGlyph);
    textarea.set_cursor_line_style(Style::default());
}

/// Run the TUI event loop against an existing terminal.
///
/// `action_rx` supplies normalized [`AppAction`]s (from the crossterm bridge in
/// production, or injected directly in tests), decoupling the loop from a live
/// terminal.
pub async fn run_tui<B>(
    terminal: &mut Terminal<B>,
    provider: Arc<dyn Provider>,
    registry: Arc<ToolRegistry>,
    initial_prompt: Option<String>,
    mut action_rx: mpsc::Receiver<AppAction>,
) -> anyhow::Result<TuiState>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let (mut agent, mut ui_rx) = AgentLoop::new(provider, registry);
    let mut state = TuiState::new();
    let mut textarea = TextArea::default();
    configure_textarea(&mut textarea);
    let cmd_tx = agent.command_sender();

    // Driver-local state (not part of TuiState): the first-press timestamp of a
    // pending double-press confirm, and the last approval button rects.
    let mut confirm_state: Option<Instant> = None;
    // Assigned on the first draw (top of the loop) before any read.
    let mut button_areas: Option<ButtonAreas>;
    // Last known mouse pointer position, updated on every mouse move/click so
    // the approval footer can highlight the button the pointer is over.
    let mut mouse_pos: Option<(u16, u16)> = None;
    // Tracks a mouse press inside the input box so a click places the cursor
    // and a drag selects text.
    let mut mouse_drag = MouseDrag::default();

    let handle = agent.run();

    // A persistent interval so the spinner tick fires independently of other
    // `select!` arms — `tokio::time::sleep` is cancelled on each iteration
    // when another arm wins, which stalls the spinner during heavy streaming.
    let mut spinner_tick = tokio::time::interval(SPINNER_TICK_INTERVAL);
    // Consume the first tick that fires immediately, so the first real tick
    // is `SPINNER_TICK_INTERVAL` from now.
    spinner_tick.tick().await;

    if let Some(prompt) = initial_prompt {
        if !prompt.is_empty() {
            state.push_user_bubble(prompt.clone(), UserMessageState::Sent);
            state.agent_mode = AgentMode::Pending;
            cmd_tx.send(AgentCommand::Prompt(prompt)).await?;
        }
    }

    loop {
        // Draw before blocking on input so the very first frame (logo + any
        // initial user bubble) renders immediately instead of waiting for the
        // first event or stream chunk.
        let hint = confirm_hint(&confirm_state, &state.agent_mode);
        let mut areas = None;
        terminal.draw(|frame| {
            areas = ui::draw(frame, &mut state, &mut textarea, hint.as_deref(), mouse_pos);
        })?;
        button_areas = areas;

        select! {
            action = action_rx.recv() => {
                let Some(action) = action else { break };
                match handle_action(
                    action, &mut state, &mut textarea,
                    &mut confirm_state, &mut mouse_drag, &mut mouse_pos, &button_areas, &cmd_tx,
                ) {
                    ActionOutcome::Quit => break,
                    ActionOutcome::Continue => {}
                }
            }
            update = ui_rx.recv() => {
                let Some(update) = update else { break };
                apply_update_with_idle_reset(&mut state, &update, &mut confirm_state);
                for _ in 0..CHANNEL_CAPACITY {
                    match ui_rx.try_recv() {
                        Ok(next) => apply_update_with_idle_reset(&mut state, &next, &mut confirm_state),
                        Err(_) => break,
                    }
                }
            }
            // Expire the confirm hint after the double-press window, even with
            // no further input.
            _ = confirm_timeout(confirm_state) => {
                confirm_state = None;
            }
            // Advance the footer spinner while the agent is busy.
            _ = spinner_tick.tick() => {
                if state.agent_mode != AgentMode::Idle {
                    state.spinner_frame = state.spinner_frame.wrapping_add(1);
                }
            }
        }
    }

    drop(cmd_tx);
    drop(agent);
    handle.await??;
    Ok(state)
}

/// Apply one `UiUpdate`, then clear the confirm state if the update caused a
/// natural transition to `Idle`.
fn apply_update_with_idle_reset(
    state: &mut TuiState,
    update: &UiUpdate,
    confirm_state: &mut Option<Instant>,
) {
    let old_mode = state.agent_mode;
    state.apply_update(update);
    if old_mode != AgentMode::Idle && state.agent_mode == AgentMode::Idle {
        *confirm_state = None;
    }
}

/// Dispatch one normalized action against the current state.
///
/// Returns `ActionOutcome::Quit` only from a confirmed `Ctrl+C` while idle.
#[allow(clippy::too_many_arguments)]
fn handle_action(
    action: AppAction,
    state: &mut TuiState,
    textarea: &mut TextArea<'_>,
    confirm_state: &mut Option<Instant>,
    mouse_drag: &mut MouseDrag,
    mouse_pos: &mut Option<(u16, u16)>,
    button_areas: &Option<ButtonAreas>,
    cmd_tx: &mpsc::Sender<AgentCommand>,
) -> ActionOutcome {
    // Confirm expiry: an expired confirm counts as a fresh first press.
    if let Some(t) = *confirm_state {
        if t.elapsed() > CONFIRM_TIMEOUT {
            *confirm_state = None;
        }
    }

    match action {
        AppAction::CtrlC => handle_ctrl_c(state, confirm_state, cmd_tx),
        AppAction::Escape => handle_escape(state, textarea, confirm_state, cmd_tx),
        AppAction::ShowHelp => {
            state.show_help = !state.show_help;
            ActionOutcome::Continue
        }
        // OS SIGINT: interrupt the current round immediately, no double-press.
        AppAction::SigInt => {
            let _ = cmd_tx.try_send(AgentCommand::Stop {
                reason: "aborted by user".into(),
            });
            ActionOutcome::Continue
        }
        AppAction::Resize { .. } => ActionOutcome::Continue,
        AppAction::Paste(text) => handle_paste(text, state, textarea),
        AppAction::KeyInput(event) => {
            handle_key_input(event, state, textarea, confirm_state, cmd_tx)
        }
        AppAction::MouseInput(mouse) => {
            // Keep mouse_pos up-to-date for every mouse event so the hover
            // highlight in the approval footer reflects the pointer location.
            *mouse_pos = Some((mouse.column, mouse.row));
            handle_mouse_input(mouse, state, textarea, mouse_drag, button_areas, cmd_tx)
        }
    }
}

fn handle_paste(text: String, state: &mut TuiState, textarea: &mut TextArea<'_>) -> ActionOutcome {
    state.exit_browsing(&textarea.lines().join("\n"));
    let paste_lines: Vec<&str> = text.split('\n').collect();
    for (index, line) in paste_lines.iter().enumerate() {
        for character in line.chars() {
            textarea.input(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(character),
                crossterm::event::KeyModifiers::NONE,
            ));
        }
        // Insert a newline after each line except the last, using Shift+Enter
        // so the textarea inserts a line break rather than submitting the text.
        if index + 1 < paste_lines.len() {
            textarea.input(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::SHIFT,
            ));
        }
    }
    ActionOutcome::Continue
}

fn handle_ctrl_c(
    state: &mut TuiState,
    confirm_state: &mut Option<Instant>,
    cmd_tx: &mpsc::Sender<AgentCommand>,
) -> ActionOutcome {
    // Tool-pending: Ctrl+C denies immediately, same as Esc or the Deny button.
    if state.pending_approval.is_some() {
        if let Some((id, _, _)) = state.pending_approval.take() {
            let _ = cmd_tx.try_send(AgentCommand::DenyTool {
                id,
                reason: "denied by user".into(),
            });
        }
        return ActionOutcome::Continue;
    }
    // Busy/streaming: double-press → Stop.
    if state.agent_mode != AgentMode::Idle {
        return confirm_or_stop(confirm_state, cmd_tx);
    }
    // Idle: double-press → quit.
    match confirm_state {
        None => {
            *confirm_state = Some(Instant::now());
            ActionOutcome::Continue
        }
        Some(_) => {
            *confirm_state = None;
            ActionOutcome::Quit
        }
    }
}

fn handle_escape(
    state: &mut TuiState,
    textarea: &mut TextArea<'_>,
    confirm_state: &mut Option<Instant>,
    cmd_tx: &mpsc::Sender<AgentCommand>,
) -> ActionOutcome {
    if state.pending_approval.is_some() {
        // Approval sub-mode: Esc denies the tool immediately.
        if let Some((id, _, _)) = state.pending_approval.take() {
            let _ = cmd_tx.try_send(AgentCommand::DenyTool {
                id,
                reason: "denied by user".into(),
            });
        }
        return ActionOutcome::Continue;
    }
    if state.show_help {
        // Close the popup; confirm state is unchanged.
        state.show_help = false;
        return ActionOutcome::Continue;
    }
    if state.agent_mode == AgentMode::Idle {
        // Idle: clear input only (no confirm, no quit).
        *textarea = TextArea::default();
        configure_textarea(textarea);
        *confirm_state = None;
        return ActionOutcome::Continue;
    }
    // Busy: same key-agnostic double-press → Stop as Ctrl+C.
    confirm_or_stop(confirm_state, cmd_tx)
}

/// Double-press confirm → `AgentCommand::Stop` (busy/tool-pending path).
fn confirm_or_stop(
    confirm_state: &mut Option<Instant>,
    cmd_tx: &mpsc::Sender<AgentCommand>,
) -> ActionOutcome {
    match confirm_state {
        None => {
            *confirm_state = Some(Instant::now());
            ActionOutcome::Continue
        }
        Some(_) => {
            *confirm_state = None;
            let _ = cmd_tx.try_send(AgentCommand::Stop {
                reason: "aborted by user".into(),
            });
            ActionOutcome::Continue
        }
    }
}

fn handle_key_input(
    event: crossterm::event::KeyEvent,
    state: &mut TuiState,
    textarea: &mut TextArea<'_>,
    confirm_state: &mut Option<Instant>,
    cmd_tx: &mpsc::Sender<AgentCommand>,
) -> ActionOutcome {
    use crossterm::event::{KeyCode, KeyModifiers};

    // Approval sub-mode: Enter approves; Esc is handled by handle_escape; all
    // other keys edit the input.
    if state.pending_approval.is_some() {
        if let KeyCode::Enter = event.code {
            if let Some((id, _, _)) = state.pending_approval.take() {
                let _ = cmd_tx.try_send(AgentCommand::ApproveTool { id });
            }
            return ActionOutcome::Continue;
        }
    }

    match event.code {
        // Shift+Enter / Alt+Enter insert a newline rather than sending.
        KeyCode::Enter
            if event.modifiers.contains(KeyModifiers::SHIFT)
                || event.modifiers.contains(KeyModifiers::ALT) =>
        {
            state.exit_browsing(&textarea.lines().join("\n"));
            textarea.input(event);
            ActionOutcome::Continue
        }
        KeyCode::Enter => {
            let raw_text = textarea.lines().join("\n");
            if raw_text.trim().is_empty() {
                return ActionOutcome::Continue;
            }
            // Strip leading/trailing blank lines and common indentation so the
            // bubble and the provider both receive clean, dedented text.
            let text = dedent(&raw_text);
            let queued = state.agent_mode != AgentMode::Idle;
            let bubble_state = if queued {
                UserMessageState::Queued
            } else {
                UserMessageState::Sent
            };
            state.push_user_bubble(text.clone(), bubble_state);
            // Fresh turn sent while idle: transition to Pending so any further
            // message typed before StreamStarted is recognised as a steer.
            if !queued {
                state.agent_mode = AgentMode::Pending;
            }
            *confirm_state = None;
            let _ = cmd_tx.try_send(AgentCommand::Prompt(text));
            *textarea = TextArea::default();
            configure_textarea(textarea);
            ActionOutcome::Continue
        }
        KeyCode::PageUp => {
            state.scroll_up(3);
            ActionOutcome::Continue
        }
        KeyCode::PageDown => {
            state.scroll_down(3);
            ActionOutcome::Continue
        }
        // Home: move to the start of the current line, wrapping to the end of
        // the previous line when already at column 0. Also handles Fn+Left and
        // ZED / iTerm2 terminals that translate Cmd+Left to Home (those
        // terminals intercept Cmd+Left for their own tab navigation).
        KeyCode::Home => {
            let cursor = textarea.cursor();
            let (row, col) = (cursor.0, cursor.1);
            let lines = textarea.lines();
            if col == 0 && row > 0 {
                textarea.move_cursor(ratatui_textarea::CursorMove::Jump(
                    (row - 1) as u16,
                    lines[row - 1].chars().count() as u16,
                ));
            } else {
                textarea.move_cursor(ratatui_textarea::CursorMove::Head);
            }
            ActionOutcome::Continue
        }
        // End: move to the end of the current line, wrapping to the start of
        // the next line when already at the line end. Also handles Fn+Right.
        // Cmd+Right is intentionally not handled: it is intercepted by iTerm2
        // (tab switching) and ZED before reaching the terminal app.
        KeyCode::End => {
            let cursor = textarea.cursor();
            let (row, col) = (cursor.0, cursor.1);
            let lines = textarea.lines();
            let at_line_end = col >= lines[row].chars().count();
            if at_line_end && row + 1 < lines.len() {
                textarea.move_cursor(ratatui_textarea::CursorMove::Jump((row + 1) as u16, 0));
            } else {
                textarea.move_cursor(ratatui_textarea::CursorMove::End);
            }
            ActionOutcome::Continue
        }
        KeyCode::Up => {
            let cursor_row = textarea.cursor().0;
            if cursor_row == 0 {
                let current = textarea.lines().join("\n");
                if let Some(loaded) = state.navigate_history(HistoryDirection::Up, &current) {
                    *textarea = TextArea::from(vec![loaded]);
                    configure_textarea(textarea);
                    textarea.move_cursor(ratatui_textarea::CursorMove::End);
                }
            } else {
                textarea.input(event);
            }
            ActionOutcome::Continue
        }
        KeyCode::Down => {
            let cursor_row = textarea.cursor().0;
            let line_count = textarea.lines().len().max(1);
            if cursor_row + 1 >= line_count {
                let current = textarea.lines().join("\n");
                if let Some(loaded) = state.navigate_history(HistoryDirection::Down, &current) {
                    *textarea = TextArea::from(vec![loaded]);
                    configure_textarea(textarea);
                    textarea.move_cursor(ratatui_textarea::CursorMove::End);
                }
            } else {
                textarea.input(event);
            }
            ActionOutcome::Continue
        }
        _ => {
            // Any edit keypress while browsing copies the entry to a fresh
            // draft and exits browsing mode.
            state.exit_browsing(&textarea.lines().join("\n"));
            textarea.input(event);
            ActionOutcome::Continue
        }
    }
}

fn handle_mouse_input(
    mouse: crossterm::event::MouseEvent,
    state: &mut TuiState,
    textarea: &mut TextArea<'_>,
    mouse_drag: &mut MouseDrag,
    button_areas: &Option<ButtonAreas>,
    cmd_tx: &mpsc::Sender<AgentCommand>,
) -> ActionOutcome {
    use crossterm::event::MouseEventKind;

    if state.pending_approval.is_some() {
        // Scroll events are allowed even during pending approval so the user
        // can review earlier conversation context before deciding.
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                state.scroll_up(1);
                return ActionOutcome::Continue;
            }
            MouseEventKind::ScrollDown => {
                state.scroll_down(1);
                return ActionOutcome::Continue;
            }
            _ => {}
        }

        if let MouseEventKind::Down(button) = mouse.kind {
            if button == crossterm::event::MouseButton::Left {
                let position = ratatui::layout::Position::new(mouse.column, mouse.row);
                if let Some(areas) = button_areas {
                    if areas.approve.contains(position) {
                        if let Some((id, _, _)) = state.pending_approval.take() {
                            let _ = cmd_tx.try_send(AgentCommand::ApproveTool { id });
                        }
                        return ActionOutcome::Continue;
                    }
                    if areas.deny.contains(position) {
                        if let Some((id, _, _)) = state.pending_approval.take() {
                            let _ = cmd_tx.try_send(AgentCommand::DenyTool {
                                id,
                                reason: "denied by user".into(),
                            });
                        }
                        return ActionOutcome::Continue;
                    }
                }
            }
        }
        // All other events outside the approval buttons are a no-op.
        return ActionOutcome::Continue;
    }

    // The chat area scrolls directly with the mouse wheel — no focus needed.
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            state.scroll_up(1);
            return ActionOutcome::Continue;
        }
        MouseEventKind::ScrollDown => {
            state.scroll_down(1);
            return ActionOutcome::Continue;
        }
        _ => {}
    }

    // Determine whether the click lands in the input box or the chat area.
    let input_area = state.input_area;
    let in_input = cursor_for_click(textarea, input_area, mouse.column, mouse.row).is_some();
    let in_chat = state
        .chat_area
        .contains(ratatui::layout::Position::new(mouse.column, mouse.row));

    // A click inside the input box places the cursor; a drag selects text.
    let area = state.input_area;
    match mouse.kind {
        MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
            if in_input {
                if let Some((row, col)) = cursor_for_click(textarea, area, mouse.column, mouse.row)
                {
                    state.exit_browsing(&textarea.lines().join("\n"));
                    textarea
                        .move_cursor(ratatui_textarea::CursorMove::Jump(row as u16, col as u16));
                    textarea.cancel_selection();
                    *mouse_drag = MouseDrag {
                        active: true,
                        moved: false,
                    };
                }
            } else if in_chat {
                // Start a chat selection anchored at the clicked position.
                let anchor_line = state
                    .scroll_offset
                    .saturating_add((mouse.row.saturating_sub(state.chat_area.y)) as usize);
                let anchor_col = (mouse.column.saturating_sub(state.chat_area.x)) as usize;
                state.chat_selection = Some(ChatSelection {
                    anchor_line,
                    anchor_col,
                    cursor_line: anchor_line,
                    cursor_col: anchor_col,
                });
                state.chat_drag_active = true;
            }
        }
        MouseEventKind::Drag(crossterm::event::MouseButton::Left) => {
            if mouse_drag.active {
                if let Some((row, col)) = cursor_for_click(textarea, area, mouse.column, mouse.row)
                {
                    // Start the selection on the first drag movement, then
                    // extend it as the cursor moves.
                    if !mouse_drag.moved {
                        textarea.start_selection();
                        mouse_drag.moved = true;
                    }
                    textarea
                        .move_cursor(ratatui_textarea::CursorMove::Jump(row as u16, col as u16));
                }
            } else if state.chat_drag_active {
                // Update the cursor end of the chat selection.
                if let Some(sel) = state.chat_selection.as_mut() {
                    sel.cursor_line = state
                        .scroll_offset
                        .saturating_add((mouse.row.saturating_sub(state.chat_area.y)) as usize);
                    sel.cursor_col = (mouse.column.saturating_sub(state.chat_area.x)) as usize;
                }
            }
        }
        MouseEventKind::Up(crossterm::event::MouseButton::Left) => {
            // If the input drag moved (selection was made), copy selected text to the
            // OS clipboard and show a toast. `textarea.copy()` yanks the
            // selection into the internal buffer and clears the selection
            // highlight — exactly what we want on mouse-button release.
            if mouse_drag.moved {
                textarea.copy();
                let yanked = textarea.yank_text();
                if !yanked.is_empty() {
                    match arboard::Clipboard::new() {
                        Ok(mut clipboard) => {
                            if clipboard.set_text(yanked).is_ok() {
                                state.set_toast("Copied to clipboard");
                            }
                        }
                        Err(error) => {
                            tracing::warn!(?error, "clipboard unavailable");
                        }
                    }
                }
            }
            *mouse_drag = MouseDrag::default();

            // Finalise chat drag selection and copy to clipboard.
            if state.chat_drag_active {
                state.chat_drag_active = false;
                if let Some(selected_text) = state.extract_chat_selection_text() {
                    if !selected_text.is_empty() {
                        match arboard::Clipboard::new() {
                            Ok(mut clipboard) => {
                                if clipboard.set_text(selected_text).is_ok() {
                                    state.set_toast("Copied to clipboard");
                                }
                            }
                            Err(error) => {
                                tracing::warn!(?error, "clipboard unavailable");
                            }
                        }
                    }
                } else {
                    // Plain click (no drag) — discard the anchor so no stale
                    // highlight lingers on the next render frame.
                    state.chat_selection = None;
                }
            }
        }
        _ => {}
    }
    ActionOutcome::Continue
}

/// Tracks a mouse press inside the input box so a click can be distinguished
/// from a drag (which selects text).
#[derive(Debug, Clone, Copy, Default)]
struct MouseDrag {
    active: bool,
    moved: bool,
}

/// Dedent `text`: remove the common leading whitespace from all non-empty
/// lines (via `textwrap::dedent`), then strip leading/trailing blank-only lines.
fn dedent(text: &str) -> String {
    let dedented = textwrap::dedent(text);
    let lines: Vec<&str> = dedented.lines().collect();
    let Some(start) = lines.iter().position(|l| !l.trim().is_empty()) else {
        return String::new();
    };
    let end = lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map(|i| i + 1)
        .unwrap_or(lines.len());
    lines[start..end].join("\n")
}

/// Translate a mouse `(column, row)` inside the input box `area` into a
/// (line, column) cursor position in `textarea`. Word-wrapping is accounted
/// for so a click on a wrapped line lands on the right character. Returns
/// `None` when the point is outside the input box (e.g. on its border).
fn cursor_for_click(
    textarea: &TextArea<'_>,
    area: ratatui::layout::Rect,
    column: u16,
    row: u16,
) -> Option<(usize, usize)> {
    if column < area.x.saturating_add(1) || row < area.y {
        return None;
    }
    let width = area.width.saturating_sub(1).max(1) as usize;
    let screen_row = (row - area.y) as usize;
    let screen_col = (column - area.x - 1) as usize;
    let lines = textarea.lines();
    let mut row_start = 0usize;
    for (line_index, line) in lines.iter().enumerate() {
        let segments = textwrap::wrap(line, width);
        let row_count = segments.len().max(1);
        let row_end = row_start + row_count;
        if screen_row < row_end {
            let segment = screen_row.saturating_sub(row_start).min(row_count - 1);
            // Walk to the target segment, reconstructing each wrapped part's
            // real start offset in the original line (textwrap's output drops
            // the inter-word separators, so a raw length sum would under-count
            // the spaces between words).
            let mut search_from = 0usize;
            let mut start_col = 0usize;
            for (index, part) in segments.iter().take(segment + 1).enumerate() {
                let offset = line[search_from..].find(part.as_ref()).unwrap_or(0) + search_from;
                start_col += line[search_from..offset].chars().count();
                if index == segment {
                    break;
                }
                start_col += part.chars().count();
                search_from = offset + part.len();
            }
            let col = (start_col + screen_col).min(line.chars().count());
            return Some((line_index, col));
        }
        row_start = row_end;
    }
    // Clicked below the last line: place the cursor at the very end.
    let last = lines.len().saturating_sub(1);
    Some((last, lines.get(last).map_or(0, |line| line.chars().count())))
}

/// Returns the footer hint string when confirm is active, `None` otherwise.
fn confirm_hint(confirm_state: &Option<Instant>, mode: &AgentMode) -> Option<String> {
    if confirm_state.is_none() {
        return None;
    }
    let hint = if *mode == AgentMode::Idle {
        "Press Ctrl+C or Esc again to quit"
    } else {
        "Press Ctrl+C or Esc again to cancel"
    };
    Some(hint.to_string())
}

/// A future that resolves once the double-press confirm window elapses (or
/// never, when no confirm is pending). Used by the `select!` loop to expire a
/// stale confirm hint even when the user presses nothing further.
async fn confirm_timeout(confirm_state: Option<Instant>) {
    let Some(start) = confirm_state else {
        std::future::pending::<()>().await;
        return;
    };
    let remaining = (start + CONFIRM_TIMEOUT).saturating_duration_since(Instant::now());
    tokio::time::sleep(remaining).await;
}

/// Production entry point: builds a real `CrosstermBackend` terminal, enables
/// raw mode + alternate screen, installs the SIGINT handler, and guarantees
/// the terminal is restored on every exit path.
pub async fn run_tui_on_stdout(
    provider: Arc<dyn Provider>,
    registry: Arc<ToolRegistry>,
    initial_prompt: Option<String>,
) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        Hide,
        EnableMouseCapture,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
        EnableBracketedPaste,
    )?;
    // The guard restores the terminal even if the TUI panics or errors.
    let _guard = TerminalGuard;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let (action_tx, action_rx) = mpsc::channel::<AppAction>(CHANNEL_CAPACITY);
    spawn_crossterm_bridge(action_tx.clone());
    install_sigint_relay(action_tx)?;

    run_tui(&mut terminal, provider, registry, initial_prompt, action_rx)
        .await
        .map(|_| ())
}

/// RAII guard restoring the terminal to its pre-TUI state. The restoration
/// runs on drop, so it cannot be skipped by panics or early returns.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            Show,
            DisableMouseCapture,
            PopKeyboardEnhancementFlags,
            DisableBracketedPaste,
        );
        let _ = disable_raw_mode();
    }
}

/// Bridge OS SIGINT to `AppAction::SigInt` so the loop can `AgentCommand::Stop`
/// immediately (no double-press).
///
/// The `ctrlc` handler runs on a dedicated OS thread. A `spawn_blocking`
/// forwarder polls for both SIGINT and the action channel's closure so it
/// never leaks a thread that hangs runtime shutdown:
///  - SIGINT is forwarded as `AppAction::SigInt` into the *action* channel, so
///    this relay holds no `cmd_tx` clone and cannot keep the agent loop alive
///    after a quit.
///  - The forwarder exits as soon as the action receiver is dropped (the main
///    loop finished), so the blocking thread is gone before the runtime drops.
fn install_sigint_relay(action_tx: mpsc::Sender<AppAction>) -> anyhow::Result<()> {
    let (sigint_tx, sigint_rx) = std::sync::mpsc::channel::<()>();
    ctrlc::set_handler(move || {
        let _ = sigint_tx.send(());
    })?;
    tokio::task::spawn_blocking(move || {
        // Poll instead of a blocking `recv()`: the SIGINT sender is held by the
        // `ctrlc` handler for the whole process, so a plain `recv()` would
        // block forever and hang the runtime on shutdown.
        while !action_tx.is_closed() {
            match sigint_rx.recv_timeout(Duration::from_millis(50)) {
                Ok(()) => {
                    if action_tx.blocking_send(AppAction::SigInt).is_err() {
                        break;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });
    Ok(())
}

#[cfg(test)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentMode, UiUpdate};
    use crate::app::ChatEntry;
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use tokio::sync::mpsc;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn setup() -> (
        TuiState,
        TextArea<'static>,
        mpsc::Sender<AgentCommand>,
        mpsc::Receiver<AgentCommand>,
    ) {
        let (cmd_tx, cmd_rx) = mpsc::channel(16);
        (TuiState::new(), TextArea::default(), cmd_tx, cmd_rx)
    }

    #[test]
    fn idle_ctrl_c_double_press_quits() {
        let (mut state, mut textarea, cmd_tx, _cmd_rx) = setup();
        let mut confirm = None;

        let first = handle_action(
            AppAction::CtrlC,
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &None,
            &cmd_tx,
        );
        assert_eq!(first, ActionOutcome::Continue);
        assert!(confirm.is_some());

        let second = handle_action(
            AppAction::CtrlC,
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &None,
            &cmd_tx,
        );
        assert_eq!(second, ActionOutcome::Quit);
        assert!(confirm.is_none());
    }

    #[test]
    fn idle_escape_clears_input_without_quit() {
        let (mut state, _textarea, cmd_tx, _cmd_rx) = setup();
        let mut textarea = TextArea::from(vec!["draft".to_string()]);
        let mut confirm = Some(Instant::now());

        let outcome = handle_action(
            AppAction::Escape,
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &None,
            &cmd_tx,
        );
        assert_eq!(outcome, ActionOutcome::Continue);
        assert_eq!(textarea.lines(), &[""]);
        assert!(confirm.is_none());
    }

    #[test]
    fn busy_double_press_ctrl_c_sends_stop() {
        let (mut state, mut textarea, cmd_tx, mut cmd_rx) = setup();
        state.apply_update(&UiUpdate::StreamStarted);
        let mut confirm = None;

        let first = handle_action(
            AppAction::CtrlC,
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &None,
            &cmd_tx,
        );
        assert_eq!(first, ActionOutcome::Continue);
        assert!(confirm.is_some());

        let second = handle_action(
            AppAction::CtrlC,
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &None,
            &cmd_tx,
        );
        assert_eq!(second, ActionOutcome::Continue);
        assert_eq!(
            cmd_rx.try_recv(),
            Ok(AgentCommand::Stop {
                reason: "aborted by user".into()
            })
        );
    }

    #[test]
    fn tool_pending_ctrl_c_denies_tool_immediately() {
        let (mut state, mut textarea, cmd_tx, mut cmd_rx) = setup();
        let id = uuid::Uuid::new_v4();
        state.pending_approval = Some((id, "read".to_string(), serde_json::json!({})));
        let mut confirm = None;

        let outcome = handle_action(
            AppAction::CtrlC,
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &None,
            &cmd_tx,
        );
        assert_eq!(outcome, ActionOutcome::Continue);
        assert!(confirm.is_none(), "no confirm window opened");
        assert!(state.pending_approval.is_none(), "approval cleared");
        assert_eq!(
            cmd_rx.try_recv(),
            Ok(AgentCommand::DenyTool {
                id,
                reason: "denied by user".into(),
            })
        );
    }

    #[test]
    fn tool_pending_ctrl_c_does_not_send_stop() {
        let (mut state, mut textarea, cmd_tx, mut cmd_rx) = setup();
        let id = uuid::Uuid::new_v4();
        state.pending_approval = Some((id, "read".to_string(), serde_json::json!({})));
        let mut confirm = None;

        // Two presses: first denies the tool; second is now in idle-ish state
        // (pending_approval is gone) so confirm window opens — no Stop sent.
        handle_action(
            AppAction::CtrlC,
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &None,
            &cmd_tx,
        );
        // Drain the DenyTool that was sent.
        let _ = cmd_rx.try_recv();

        handle_action(
            AppAction::CtrlC,
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &None,
            &cmd_tx,
        );
        // No Stop should have been sent.
        assert_eq!(cmd_rx.try_recv(), Err(mpsc::error::TryRecvError::Empty));
    }

    #[test]
    fn key_agnostic_confirm_escape_after_ctrl_c_stops() {
        let (mut state, mut textarea, cmd_tx, mut cmd_rx) = setup();
        state.apply_update(&UiUpdate::StreamStarted);
        let mut confirm = None;

        handle_action(
            AppAction::CtrlC,
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &None,
            &cmd_tx,
        );
        let second = handle_action(
            AppAction::Escape,
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &None,
            &cmd_tx,
        );
        assert_eq!(second, ActionOutcome::Continue);
        assert_eq!(
            cmd_rx.try_recv(),
            Ok(AgentCommand::Stop {
                reason: "aborted by user".into()
            })
        );
    }

    #[test]
    fn approval_enter_sends_approve_tool() {
        let (mut state, mut textarea, cmd_tx, mut cmd_rx) = setup();
        let id = uuid::Uuid::new_v4();
        state.pending_approval = Some((id, "read".to_string(), serde_json::json!({})));
        let mut confirm = None;

        let outcome = handle_action(
            AppAction::KeyInput(key(KeyCode::Enter)),
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &None,
            &cmd_tx,
        );
        assert_eq!(outcome, ActionOutcome::Continue);
        assert_eq!(state.pending_approval, None);
        assert_eq!(cmd_rx.try_recv(), Ok(AgentCommand::ApproveTool { id }));
    }

    #[test]
    fn approval_escape_sends_deny_tool() {
        let (mut state, mut textarea, cmd_tx, mut cmd_rx) = setup();
        let id = uuid::Uuid::new_v4();
        state.pending_approval = Some((id, "read".to_string(), serde_json::json!({})));
        let mut confirm = None;

        let outcome = handle_action(
            AppAction::Escape,
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &None,
            &cmd_tx,
        );
        assert_eq!(outcome, ActionOutcome::Continue);
        assert_eq!(state.pending_approval, None);
        assert_eq!(
            cmd_rx.try_recv(),
            Ok(AgentCommand::DenyTool {
                id,
                reason: "denied by user".into()
            })
        );
    }

    #[test]
    fn approval_mouse_click_on_approve_button_sends_approve() {
        let (mut state, mut textarea, cmd_tx, mut cmd_rx) = setup();
        let id = uuid::Uuid::new_v4();
        state.pending_approval = Some((id, "read".to_string(), serde_json::json!({})));
        let mut confirm = None;
        let button_areas = Some(ButtonAreas {
            approve: ratatui::layout::Rect::new(0, 20, 10, 1),
            deny: ratatui::layout::Rect::new(11, 20, 10, 1),
        });
        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 3,
            row: 20,
            modifiers: KeyModifiers::NONE,
        };
        let outcome = handle_action(
            AppAction::MouseInput(mouse),
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &button_areas,
            &cmd_tx,
        );
        assert_eq!(outcome, ActionOutcome::Continue);
        assert_eq!(cmd_rx.try_recv(), Ok(AgentCommand::ApproveTool { id }));
    }

    #[test]
    fn approval_mouse_click_outside_buttons_is_noop() {
        let (mut state, mut textarea, cmd_tx, mut cmd_rx) = setup();
        state.pending_approval = Some((
            uuid::Uuid::new_v4(),
            "read".to_string(),
            serde_json::json!({}),
        ));
        let mut confirm = None;
        let button_areas = Some(ButtonAreas {
            approve: ratatui::layout::Rect::new(0, 20, 10, 1),
            deny: ratatui::layout::Rect::new(11, 20, 10, 1),
        });
        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 50,
            row: 20,
            modifiers: KeyModifiers::NONE,
        };
        let outcome = handle_action(
            AppAction::MouseInput(mouse),
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &button_areas,
            &cmd_tx,
        );
        assert_eq!(outcome, ActionOutcome::Continue);
        assert!(state.pending_approval.is_some());
        assert_eq!(cmd_rx.try_recv(), Err(mpsc::error::TryRecvError::Empty));
    }

    #[test]
    fn help_toggle_and_escape_closes() {
        let (mut state, mut textarea, cmd_tx, _cmd_rx) = setup();
        let mut confirm = Some(Instant::now());

        handle_action(
            AppAction::ShowHelp,
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &None,
            &cmd_tx,
        );
        assert!(state.show_help);
        handle_action(
            AppAction::Escape,
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &None,
            &cmd_tx,
        );
        assert!(!state.show_help);
        assert!(confirm.is_some());
    }

    #[test]
    fn enter_sends_prompt_as_sent_when_idle_and_queued_when_busy() {
        let (mut state, _textarea, cmd_tx, mut cmd_rx) = setup();
        let mut textarea = TextArea::from(vec!["hello".to_string()]);
        let mut confirm = None;

        handle_action(
            AppAction::KeyInput(key(KeyCode::Enter)),
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &None,
            &cmd_tx,
        );
        assert_eq!(
            cmd_rx.try_recv(),
            Ok(AgentCommand::Prompt("hello".to_string()))
        );
        assert_eq!(
            state.chat_entries[0],
            ChatEntry::UserMessage {
                text: "hello".to_string(),
                state: UserMessageState::Sent
            }
        );

        state.apply_update(&UiUpdate::StreamStarted);
        textarea = TextArea::from(vec!["steer".to_string()]);
        handle_action(
            AppAction::KeyInput(key(KeyCode::Enter)),
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &None,
            &cmd_tx,
        );
        assert_eq!(
            cmd_rx.try_recv(),
            Ok(AgentCommand::Prompt("steer".to_string()))
        );
        assert_eq!(
            state.chat_entries[1],
            ChatEntry::UserMessage {
                text: "steer".to_string(),
                state: UserMessageState::Queued
            }
        );
    }

    #[test]
    fn empty_enter_is_ignored() {
        let (mut state, mut textarea, cmd_tx, mut cmd_rx) = setup();
        let mut confirm = None;
        handle_action(
            AppAction::KeyInput(key(KeyCode::Enter)),
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &None,
            &cmd_tx,
        );
        assert_eq!(cmd_rx.try_recv(), Err(mpsc::error::TryRecvError::Empty));
        assert!(state.chat_entries.is_empty());
    }

    #[test]
    fn enter_trims_leading_and_trailing_blank_lines_from_bubble_and_prompt() {
        let (mut state, _textarea, cmd_tx, mut cmd_rx) = setup();
        let mut textarea = TextArea::from(vec![
            "".to_string(),
            "  ".to_string(),
            "hello world".to_string(),
            "".to_string(),
        ]);
        let mut confirm = None;

        handle_action(
            AppAction::KeyInput(key(KeyCode::Enter)),
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &None,
            &cmd_tx,
        );
        assert_eq!(
            cmd_rx.try_recv(),
            Ok(AgentCommand::Prompt("hello world".to_string()))
        );
        assert_eq!(
            state.chat_entries[0],
            ChatEntry::UserMessage {
                text: "hello world".to_string(),
                state: UserMessageState::Sent
            }
        );
    }

    #[test]
    fn trim_empty_lines_removes_surrounding_blank_lines() {
        assert_eq!(dedent("\n\nhello\nworld\n\n"), "hello\nworld");
        assert_eq!(dedent("  \nhello\n  "), "hello");
        assert_eq!(dedent("hello"), "hello");
        assert_eq!(dedent("\n\n"), "");
    }

    #[test]
    fn dedent_removes_common_leading_whitespace() {
        assert_eq!(dedent("   hello\n   world"), "hello\nworld");
        assert_eq!(dedent("\n   hello\n   world\n"), "hello\nworld");
        assert_eq!(dedent("   hello\n     world"), "hello\n  world");
        assert_eq!(dedent("\t\thello\n\t\tworld"), "hello\nworld");
    }

    #[test]
    fn shift_enter_inserts_newline_without_sending() {
        let (mut state, _textarea, cmd_tx, mut cmd_rx) = setup();
        let mut textarea = TextArea::from(vec!["line one".to_string()]);
        textarea.move_cursor(ratatui_textarea::CursorMove::End);
        let mut confirm = None;

        let shift_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);
        let outcome = handle_action(
            AppAction::KeyInput(shift_enter),
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &None,
            &cmd_tx,
        );
        assert_eq!(outcome, ActionOutcome::Continue);
        assert_eq!(textarea.lines(), &["line one", ""], "a new line is added");
        assert_eq!(
            cmd_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty),
            "shift+enter must not send the prompt"
        );
    }

    #[test]
    fn sigint_forwards_stop_immediately() {
        let (mut state, mut textarea, cmd_tx, mut cmd_rx) = setup();
        state.apply_update(&UiUpdate::StreamStarted);
        let mut confirm = None;

        let outcome = handle_action(
            AppAction::SigInt,
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &None,
            &cmd_tx,
        );
        assert_eq!(outcome, ActionOutcome::Continue);
        assert_eq!(
            cmd_rx.try_recv(),
            Ok(AgentCommand::Stop {
                reason: "aborted by user".into()
            })
        );
        assert!(
            confirm.is_none(),
            "OS SIGINT must not set the double-press confirm state"
        );
    }

    #[test]
    fn up_on_first_line_navigates_history() {
        let (mut state, mut textarea, cmd_tx, _cmd_rx) = setup();
        state.push_user_bubble("previous".to_string(), UserMessageState::Sent);
        let mut confirm = None;

        let outcome = handle_action(
            AppAction::KeyInput(key(KeyCode::Up)),
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &None,
            &cmd_tx,
        );
        assert_eq!(outcome, ActionOutcome::Continue);
        assert_eq!(textarea.lines(), &["previous"]);
    }

    #[test]
    fn page_down_and_page_up_scroll_chat() {
        let (mut state, mut textarea, cmd_tx, _cmd_rx) = setup();
        let mut confirm = None;

        // PageDown moves toward later content (offset increases).
        handle_action(
            AppAction::KeyInput(key(KeyCode::PageDown)),
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &None,
            &cmd_tx,
        );
        assert_eq!(state.scroll_offset, 3);
        assert!(!state.scroll_locked);

        // PageUp moves back toward earlier content (offset decreases).
        handle_action(
            AppAction::KeyInput(key(KeyCode::PageUp)),
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &None,
            &cmd_tx,
        );
        assert_eq!(state.scroll_offset, 0);
        assert!(!state.scroll_locked);
    }

    #[test]
    fn end_navigates_line_end_and_wraps_to_next_line() {
        let (mut state, _textarea, cmd_tx, _cmd_rx) = setup();
        let mut textarea = TextArea::from(vec!["abc".to_string(), "de".to_string()]);
        let mut confirm = None;
        textarea.move_cursor(ratatui_textarea::CursorMove::Top);

        // End: move to end of line 0.
        handle_action(
            AppAction::KeyInput(key(KeyCode::End)),
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &None,
            &cmd_tx,
        );
        assert_eq!(textarea.cursor(), (0, 3));

        // End again at line end: wrap to start of line 1.
        handle_action(
            AppAction::KeyInput(key(KeyCode::End)),
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &None,
            &cmd_tx,
        );
        assert_eq!(textarea.cursor(), (1, 0));

        // End on last line: move to its end (no further wrapping).
        handle_action(
            AppAction::KeyInput(key(KeyCode::End)),
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &None,
            &cmd_tx,
        );
        assert_eq!(textarea.cursor(), (1, 2));
    }

    #[test]
    fn home_key_navigates_to_line_start_and_wraps_to_prev_line() {
        let (mut state, _textarea, cmd_tx, _cmd_rx) = setup();
        let mut textarea = TextArea::from(vec!["abc".to_string(), "de".to_string()]);
        let mut confirm = None;
        // Place cursor at end of line 1.
        textarea.move_cursor(ratatui_textarea::CursorMove::Bottom);
        textarea.move_cursor(ratatui_textarea::CursorMove::End);
        assert_eq!(textarea.cursor(), (1, 2));

        // Home: move to start of line 1.
        handle_action(
            AppAction::KeyInput(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE)),
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &None,
            &cmd_tx,
        );
        assert_eq!(textarea.cursor(), (1, 0));

        // Home again at col 0: wrap to end of line 0.
        handle_action(
            AppAction::KeyInput(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE)),
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &None,
            &cmd_tx,
        );
        assert_eq!(textarea.cursor(), (0, 3));

        // Home at end of line 0: move to start of line 0 (col 0).
        handle_action(
            AppAction::KeyInput(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE)),
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &None,
            &cmd_tx,
        );
        assert_eq!(textarea.cursor(), (0, 0));

        // Home at col 0 of the first row: stays at (0, 0) (no prev row).
        handle_action(
            AppAction::KeyInput(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE)),
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &None,
            &cmd_tx,
        );
        assert_eq!(textarea.cursor(), (0, 0));
    }

    #[test]
    fn mouse_wheel_scrolls_chat() {
        let (mut state, mut textarea, cmd_tx, _cmd_rx) = setup();
        let mut confirm = None;

        let wheel_down = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        handle_action(
            AppAction::MouseInput(wheel_down),
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &None,
            &cmd_tx,
        );
        assert_eq!(state.scroll_offset, 1);
        assert!(!state.scroll_locked);

        let wheel_up = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        handle_action(
            AppAction::MouseInput(wheel_up),
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &None,
            &cmd_tx,
        );
        assert_eq!(state.scroll_offset, 0);
        assert!(!state.scroll_locked);
    }

    #[test]
    fn mouse_wheel_scrolls_chat_during_pending_approval() {
        // Scroll events must be forwarded even when a tool approval is pending.
        let (mut state, mut textarea, cmd_tx, mut cmd_rx) = setup();
        let id = uuid::Uuid::new_v4();
        state.pending_approval = Some((id, "read".to_string(), serde_json::json!({})));
        let mut confirm = None;
        let button_areas = Some(ButtonAreas {
            approve: ratatui::layout::Rect::new(0, 20, 10, 1),
            deny: ratatui::layout::Rect::new(11, 20, 10, 1),
        });

        let wheel_down = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        handle_action(
            AppAction::MouseInput(wheel_down),
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &button_areas,
            &cmd_tx,
        );
        assert_eq!(state.scroll_offset, 1);
        // Approval must still be pending — scroll did not consume it.
        assert!(state.pending_approval.is_some());
        assert_eq!(cmd_rx.try_recv(), Err(mpsc::error::TryRecvError::Empty));

        let wheel_up = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        handle_action(
            AppAction::MouseInput(wheel_up),
            &mut state,
            &mut textarea,
            &mut confirm,
            &mut MouseDrag::default(),
            &mut None,
            &button_areas,
            &cmd_tx,
        );
        assert_eq!(state.scroll_offset, 0);
        assert!(state.pending_approval.is_some());
    }

    #[test]
    fn confirm_hint_reflects_mode() {
        assert_eq!(
            confirm_hint(&Some(Instant::now()), &AgentMode::Idle).as_deref(),
            Some("Press Ctrl+C or Esc again to quit")
        );
        assert_eq!(
            confirm_hint(&Some(Instant::now()), &AgentMode::Streaming).as_deref(),
            Some("Press Ctrl+C or Esc again to cancel")
        );
        assert_eq!(confirm_hint(&None, &AgentMode::Idle), None);
    }

    #[tokio::test]
    async fn confirm_timeout_resolves_after_window_when_pending() {
        let start = Instant::now();
        tokio::time::timeout(Duration::from_secs(3), confirm_timeout(Some(start)))
            .await
            .expect("confirm timeout resolves within 3s");
    }

    #[tokio::test]
    async fn confirm_timeout_never_resolves_without_pending_confirm() {
        let result = tokio::time::timeout(Duration::from_millis(50), confirm_timeout(None)).await;
        assert!(
            result.is_err(),
            "should not resolve when no confirm is pending"
        );
    }

    fn mouse_event(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn click_in_input_places_cursor() {
        let (mut state, _empty, cmd_tx, _cmd_rx) = setup();
        let mut textarea = TextArea::from(vec!["hello world".to_string()]);
        state.input_area = ratatui::layout::Rect::new(0, 10, 30, 3);
        let mut drag = MouseDrag::default();

        handle_mouse_input(
            mouse_event(MouseEventKind::Down(MouseButton::Left), 5, 10),
            &mut state,
            &mut textarea,
            &mut drag,
            &None,
            &cmd_tx,
        );
        assert_eq!(
            textarea.cursor(),
            (0, 4),
            "click at column 5 lands on the 4th character (after the border)"
        );
        assert!(
            drag.active,
            "a press inside the input begins a potential drag"
        );
        assert!(
            !textarea.is_selecting(),
            "a plain click must not leave a selection"
        );
    }

    #[test]
    fn drag_in_input_selects_text() {
        let (mut state, _empty, cmd_tx, _cmd_rx) = setup();
        let mut textarea = TextArea::from(vec!["hello world".to_string()]);
        state.input_area = ratatui::layout::Rect::new(0, 10, 30, 3);
        let mut drag = MouseDrag::default();

        handle_mouse_input(
            mouse_event(MouseEventKind::Down(MouseButton::Left), 5, 10),
            &mut state,
            &mut textarea,
            &mut drag,
            &None,
            &cmd_tx,
        );
        handle_mouse_input(
            mouse_event(MouseEventKind::Drag(MouseButton::Left), 10, 10),
            &mut state,
            &mut textarea,
            &mut drag,
            &None,
            &cmd_tx,
        );
        assert!(
            textarea.is_selecting(),
            "a drag extends the selection across the swept characters"
        );
        assert_eq!(textarea.cursor(), (0, 9));

        handle_mouse_input(
            mouse_event(MouseEventKind::Up(MouseButton::Left), 10, 10),
            &mut state,
            &mut textarea,
            &mut drag,
            &None,
            &cmd_tx,
        );
        assert!(!drag.active, "mouse release ends the drag");
    }

    #[test]
    fn click_on_input_border_is_ignored() {
        let (mut state, _empty, cmd_tx, _cmd_rx) = setup();
        let mut textarea = TextArea::from(vec!["hello".to_string()]);
        state.input_area = ratatui::layout::Rect::new(0, 10, 30, 3);
        let mut drag = MouseDrag::default();

        handle_mouse_input(
            mouse_event(MouseEventKind::Down(MouseButton::Left), 0, 10),
            &mut state,
            &mut textarea,
            &mut drag,
            &None,
            &cmd_tx,
        );
        assert!(!drag.active, "clicking the border column does nothing");
        assert_eq!(textarea.cursor(), (0, 0));
    }

    #[test]
    fn cursor_for_click_maps_wrapped_lines() {
        let textarea = TextArea::from(vec!["aaaa bbbb cccc".to_string()]);
        let area = ratatui::layout::Rect::new(0, 0, 7, 10); // width 6 after border
                                                            // Third wrapped segment ("cccc"), column 1 -> "cccc" starts at char 10.
        assert_eq!(
            cursor_for_click(&textarea, area, 2, 2),
            Some((0, 11)),
            "click on a wrapped segment maps to the right character"
        );
        // Second wrapped segment ("bbbb"), column 0.
        assert_eq!(cursor_for_click(&textarea, area, 1, 1), Some((0, 5)));
        // Clicking the left border returns None.
        assert_eq!(cursor_for_click(&textarea, area, 0, 0), None);
    }

    #[test]
    fn cursor_for_click_below_last_line_returns_end() {
        let textarea = TextArea::from(vec!["hi".to_string()]);
        let area = ratatui::layout::Rect::new(0, 0, 7, 10);
        assert_eq!(
            cursor_for_click(&textarea, area, 3, 9),
            Some((0, 2)),
            "clicking below the last line places the cursor at the end"
        );
    }
}
