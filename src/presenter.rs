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

use crate::agent::tools::clarify::ClarifyTool;
use crate::markdown::render_markdown;
use crate::slash_command::{parse_slash_command_invocation, SlashCommand, SlashCommandInvocation};
use crate::tui_reducer_state::{
    model_picker_row_entries, slash_overlay_text_rows, AppState, ChatEntry, ChatSelection,
    ChildRunInfo, ConfirmKey, ContextUsage, ConversationView, DomainEvent, Effect, Focus,
    HistoryDirection, InputEvent, SlashOverlay, ToolCallState, TuiMode, UserMessageState,
    HELP_HORIZONTAL_SCROLL_COLUMNS,
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
            // Closing help resumes an outstanding tool approval rather than
            // the main input — otherwise the slash-command-capable input
            // would take over while a tool still awaits its verdict.
            state.restore_focus_after_help();
            state.help_popup.help_scroll_offset = 0;
            state.help_popup.help_scroll_x = 0;
            state.clear_help_selection();
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
                    state.help_scroll_left(HELP_HORIZONTAL_SCROLL_COLUMNS);
                }
                KeyCode::Right => {
                    state.help_scroll_right(HELP_HORIZONTAL_SCROLL_COLUMNS);
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
            // No reason given — deny silently without an implicit user message.
            vec![
                Effect::DenyTool {
                    id: tool_id,
                    reason: String::new(),
                },
                Effect::RestoreInputState,
            ]
        }
        InputEvent::SigInt => vec![Effect::StopAgent],
        InputEvent::KeyInput(key_event) if key_event.code == KeyCode::Enter => {
            state.focus = Focus::MainInput;
            if textarea.is_empty {
                vec![
                    Effect::ApproveTool { id: tool_id },
                    Effect::RestoreInputState,
                ]
            } else {
                // User typed a denial reason — pass it to the agent so it can
                // be forwarded to the model as an implicit user message.
                let reason = textarea.text.clone();
                vec![
                    Effect::DenyTool {
                        id: tool_id,
                        reason,
                    },
                    Effect::RestoreInputState,
                ]
            }
        }
        InputEvent::KeyInput(key_event) => vec![Effect::TextareaKey(key_event)],
        InputEvent::ShowHelp => {
            state.focus = Focus::HelpPopup;
            state.clear_help_selection();
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
    if matches!(state.mode, TuiMode::ClarifyPending { .. }) {
        return keyboard_clarify(state, event, textarea);
    }
    match event {
        InputEvent::ShowHelp => {
            state.focus = Focus::HelpPopup;
            state.clear_help_selection();
            vec![]
        }
        InputEvent::SigInt => vec![Effect::StopAgent],
        InputEvent::CtrlC => handle_ctrl_c_generic(state, textarea.is_empty),
        // Esc with an empty draft while viewing a subagent's drill-in
        // transcript navigates back to the root conversation instead of
        // acting on the (root) agent's `TuiMode` — the subagent's own
        // transcript is read-only here, so there is nothing to "cancel". A
        // non-empty draft still just clears via the normal Escape path
        // below, same as in the root view.
        InputEvent::Escape
            if textarea.is_empty && matches!(state.active_view, ConversationView::Child { .. }) =>
        {
            state.switch_to_root_view();
            vec![]
        }
        InputEvent::Escape => handle_escape_main(state, textarea),
        InputEvent::Paste(text) => vec![Effect::Paste(text)],
        InputEvent::KeyInput(key_event) => handle_key_main(state, key_event, textarea),
    }
}

/// Extracts the plain text from the clarify banner rows between `from_row` and
/// `to_row` (absolute screen rows, inclusive). Used when the user drags to
/// select text in the banner.
fn extract_clarify_banner_text(
    state: &AppState,
    from_row: u16,
    from_col: u16,
    to_row: u16,
    to_col: u16,
) -> String {
    let TuiMode::ClarifyPending {
        ref question,
        ref choices,
        ref context,
        ..
    } = state.mode
    else {
        return String::new();
    };
    let banner = state.layout.clarify_banner;
    if banner.width == 0 {
        return String::new();
    }
    let wrap_width = banner.width as usize;
    let mut all_lines: Vec<String> = Vec::new();
    if let Some(ctx) = context.as_deref() {
        for md_line in render_markdown(ctx, wrap_width) {
            all_lines.push(md_line.spans.iter().map(|s| s.content.as_ref()).collect());
        }
        all_lines.push(String::new()); // spacer between context and question
    }
    for md_line in render_markdown(question, wrap_width) {
        all_lines.push(md_line.spans.iter().map(|s| s.content.as_ref()).collect());
    }
    if let Some(ref choices_list) = choices {
        for (i, c) in choices_list.iter().enumerate() {
            all_lines.push(format!("{}. {}", i + 1, c));
        }
    }

    // Normalise drag direction so (start, start_col) is always before (end, end_col).
    let (start_row, start_col, end_row, end_col) = if (from_row, from_col) <= (to_row, to_col) {
        (from_row, from_col, to_row, to_col)
    } else {
        (to_row, to_col, from_row, from_col)
    };

    let min_row = (start_row.saturating_sub(banner.y) as usize)
        .saturating_add(state.clarify_banner_state.clarify_scroll_offset);
    let max_row = ((end_row.saturating_sub(banner.y) as usize)
        .saturating_add(state.clarify_banner_state.clarify_scroll_offset))
    .min(all_lines.len().saturating_sub(1));
    let start_col_offset = start_col.saturating_sub(banner.x) as usize;
    let end_col_offset = end_col.saturating_sub(banner.x) as usize;

    if min_row >= all_lines.len() {
        return String::new();
    }

    let selected: Vec<String> = all_lines[min_row..=max_row]
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let chars: Vec<char> = line.chars().collect();
            let line_len = chars.len();
            let (col_start, col_end) = if min_row == max_row {
                // Single-row drag: apply both column offsets.
                (start_col_offset.min(line_len), end_col_offset.min(line_len))
            } else if index == 0 {
                // First line of a multi-row drag: trim from start column.
                (start_col_offset.min(line_len), line_len)
            } else if index == max_row - min_row {
                // Last line of a multi-row drag: trim to end column.
                (0, end_col_offset.min(line_len))
            } else {
                // Middle lines: include in full.
                (0, line_len)
            };
            chars[col_start..col_end].iter().collect()
        })
        .collect();

    selected.join("\n")
}

/// Extract the plain text from slash-overlay rows between two screen
/// positions. The two leading marker columns are presentation chrome, so
/// copied text starts at the row content rather than including the marker.
fn extract_slash_overlay_text(
    state: &AppState,
    from_column: u16,
    from_row: u16,
    to_column: u16,
    to_row: u16,
) -> String {
    let area = state.layout.slash_overlay.content_area;
    if area.is_empty() {
        return String::new();
    }
    let rows = slash_overlay_text_rows(&state.slash_overlay);
    if rows.is_empty() {
        return String::new();
    }

    let position = |column: u16, row: u16| {
        let last_row = area.y.saturating_add(area.height.saturating_sub(1));
        let clamped_row = row.clamp(area.y, last_row);
        let clamped_column = column.clamp(area.x, area.x.saturating_add(area.width));
        let row_index = state.layout.slash_overlay.first_visible_row
            + clamped_row.saturating_sub(area.y) as usize;
        let content_column = clamped_column.saturating_sub(area.x).saturating_sub(2) as usize;
        (row_index, content_column)
    };

    let start = position(from_column, from_row);
    let end = position(to_column, to_row);
    let (start, end) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    if start.0 >= rows.len() {
        return String::new();
    }
    let end_row = end.0.min(rows.len().saturating_sub(1));

    rows[start.0..=end_row]
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let characters: Vec<char> = line.chars().collect();
            let line_length = characters.len();
            let (column_start, column_end) = if start.0 == end_row {
                (start.1.min(line_length), end.1.min(line_length))
            } else if index == 0 {
                (start.1.min(line_length), line_length)
            } else if index == end_row - start.0 {
                (0, end.1.min(line_length))
            } else {
                (0, line_length)
            };
            characters[column_start..column_end].iter().collect()
        })
        .collect::<Vec<String>>()
        .join("\n")
}

fn keyboard_clarify(
    state: &mut AppState,
    event: InputEvent,
    textarea: &TextareaContext,
) -> Vec<Effect> {
    match event {
        InputEvent::ShowHelp => {
            state.focus = Focus::HelpPopup;
            state.clear_help_selection();
            vec![]
        }
        InputEvent::SigInt => vec![Effect::StopAgent],
        InputEvent::CtrlC => {
            state.mode = TuiMode::Pending;
            vec![Effect::AbortClarify, Effect::RestoreInputState]
        }
        InputEvent::Escape => {
            if textarea.is_empty {
                state.mode = TuiMode::Pending;
                vec![Effect::AbortClarify, Effect::RestoreInputState]
            } else {
                vec![Effect::ClearTextarea]
            }
        }
        InputEvent::Paste(text) => vec![Effect::Paste(text)],
        InputEvent::KeyInput(key_event) if key_event.code == KeyCode::Enter => {
            state.mode = TuiMode::Pending;
            vec![
                Effect::AnswerClarify(textarea.text.clone()),
                Effect::RestoreInputState,
            ]
        }
        InputEvent::KeyInput(key_event) if key_event.code == KeyCode::Up => {
            state.clarify_scroll_up(1);
            vec![]
        }
        InputEvent::KeyInput(key_event) if key_event.code == KeyCode::Down => {
            state.clarify_scroll_down(1);
            vec![]
        }
        InputEvent::KeyInput(key_event) if key_event.code == KeyCode::PageUp => {
            state.clarify_scroll_up(5);
            vec![]
        }
        InputEvent::KeyInput(key_event) if key_event.code == KeyCode::PageDown => {
            state.clarify_scroll_down(5);
            vec![]
        }
        InputEvent::KeyInput(key_event) => vec![Effect::TextareaKey(key_event)],
    }
}

/// Returns the mode a `DoublePressAbort` wraps, or `mode` itself when it
/// isn't one. Used to detect the *real* underlying state (e.g. truly Idle)
/// even while a confirm window armed by the other key is pending.
fn effective_mode(mode: &TuiMode) -> &TuiMode {
    match mode {
        TuiMode::DoublePressAbort { return_to, .. } => return_to,
        other => other,
    }
}

/// Ctrl+C logic for non-approval contexts.
fn handle_ctrl_c_generic(state: &mut AppState, textarea_is_empty: bool) -> Vec<Effect> {
    let mode_is_idle = matches!(effective_mode(&state.mode), TuiMode::Idle);

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
        TuiMode::DoublePressAbort {
            return_to,
            armed_by: ConfirmKey::CtrlC,
            ..
        } => {
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
        TuiMode::DoublePressAbort {
            return_to,
            armed_by: ConfirmKey::Escape,
            ..
        } => {
            // Esc armed the pending confirm, not Ctrl-C: this Ctrl-C press
            // does not confirm Esc's action. Start Ctrl-C's own fresh
            // confirm window against the real underlying mode instead.
            state.mode = TuiMode::DoublePressAbort {
                pressed_at: Instant::now(),
                return_to,
                armed_by: ConfirmKey::CtrlC,
            };
            vec![]
        }
        mode => {
            // First press: enter double-press abort mode.
            state.mode = TuiMode::DoublePressAbort {
                pressed_at: Instant::now(),
                return_to: Box::new(mode),
                armed_by: ConfirmKey::CtrlC,
            };
            vec![]
        }
    }
}

fn handle_escape_main(state: &mut AppState, textarea: &TextareaContext) -> Vec<Effect> {
    // An open slash overlay is the innermost dismissible thing on screen, so
    // it consumes Esc before the draft-clearing / abort-confirm ladder.
    //
    // Every slash overlay (including `Suggestions`) is purely a function of
    // the current draft text (see `AppState::refresh_slash_overlay`, called
    // by the Driver after every action). Hiding the overlay here without
    // also clearing the draft would be undone on the very next recompute —
    // the same still-matching text would immediately reopen it, making Esc
    // look like a no-op. So Esc always clears the draft alongside hiding
    // the overlay, matching the plain "Esc clears a non-empty draft"
    // behavior below.
    if state.slash_overlay.is_visible() {
        state.slash_overlay = SlashOverlay::Hidden;
        state.history_index = None;
        state.draft = String::new();
        return vec![Effect::ClearTextarea];
    }

    let mode_is_idle = matches!(effective_mode(&state.mode), TuiMode::Idle);

    if mode_is_idle {
        // Idle: clear textarea and reset history. This always executes
        // immediately (single press, never quits) even when a Ctrl-C
        // confirm window is pending — Esc dismisses that pending quit
        // rather than confirming it.
        state.history_index = None;
        state.draft = String::new();
        state.mode = TuiMode::Idle;
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
        TuiMode::DoublePressAbort {
            return_to,
            armed_by: ConfirmKey::Escape,
            ..
        } => {
            // Enter Aborting for visual feedback ("Aborting…" in footer +
            // spinner) but leave aborting_exit = false so the Driver stays open.
            state.mode = TuiMode::Aborting;
            let _ = return_to;
            vec![Effect::StopAgent]
        }
        TuiMode::DoublePressAbort {
            return_to,
            armed_by: ConfirmKey::CtrlC,
            ..
        } => {
            // Ctrl-C armed the pending confirm, not Esc: this Esc press
            // does not confirm Ctrl-C's quit. Start Esc's own fresh
            // confirm window against the real underlying mode instead.
            state.mode = TuiMode::DoublePressAbort {
                pressed_at: Instant::now(),
                return_to,
                armed_by: ConfirmKey::Escape,
            };
            vec![]
        }
        mode => {
            state.mode = TuiMode::DoublePressAbort {
                pressed_at: Instant::now(),
                return_to: Box::new(mode),
                armed_by: ConfirmKey::Escape,
            };
            vec![]
        }
    }
}

/// Translate a recognized slash command into its effects.
///
/// `/tools` and `/clear` clear the input first so the command line never
/// lingers as a draft. `/models` instead normalizes the draft to the
/// canonical `/models <filter>` form and leaves it in the textarea: the
/// picker it opens stays keyed off that text, so further typing narrows the
/// already-fetched catalog live instead of being silently dropped. None of
/// these commands commits a user bubble: a command is an instruction to the
/// TUI, not a turn in the conversation.
fn slash_command_effects(invocation: SlashCommandInvocation) -> Vec<Effect> {
    match invocation.command {
        SlashCommand::Models => {
            let filter = invocation.argument;
            let normalized_draft = format!("/models {}", filter.clone().unwrap_or_default());
            vec![
                Effect::SetInputText(normalized_draft),
                Effect::ListModels { filter },
            ]
        }
        SlashCommand::Tools => vec![Effect::ClearTextarea, Effect::ShowTools],
        SlashCommand::Clear => vec![Effect::ClearTextarea, Effect::ClearConversation],
    }
}

const SLASH_OVERLAY_PAGE_SIZE: isize = 3;

fn handle_key_main(
    state: &mut AppState,
    key_event: crossterm::event::KeyEvent,
    textarea: &TextareaContext,
) -> Vec<Effect> {
    // The command suggestion overlay owns navigation, completion, and plain
    // Enter while it is open. Tab replaces the partial command with the
    // canonical name of the highlighted entry; Enter activates that entry.
    // The normal textarea handler remains available when no command
    // suggestions are shown.
    if matches!(state.slash_overlay, SlashOverlay::Suggestions { .. }) {
        match key_event.code {
            KeyCode::Up => {
                state.move_slash_selection(-1);
                return vec![];
            }
            KeyCode::Down => {
                state.move_slash_selection(1);
                return vec![];
            }
            KeyCode::Tab => {
                if let Some(command) = state.selected_slash_command() {
                    return vec![Effect::SetInputText(command.name.to_string())];
                }
            }
            KeyCode::Enter if key_event.modifiers.is_empty() => {
                if let Some(command) = state.selected_slash_command().map(|info| info.command) {
                    state.slash_overlay = SlashOverlay::Hidden;
                    return slash_command_effects(SlashCommandInvocation {
                        command,
                        argument: None,
                    });
                }
            }
            _ => {}
        }
    }

    // The model picker owns navigation, reasoning-effort selection, and
    // confirmation while it is open; every other key (including plain
    // typing) falls through to the normal input handling.
    if matches!(state.slash_overlay, SlashOverlay::ModelPicker { .. }) {
        match key_event.code {
            KeyCode::Up => {
                state.move_slash_selection(-1);
                return vec![];
            }
            KeyCode::Down => {
                state.move_slash_selection(1);
                return vec![];
            }
            KeyCode::PageUp => {
                state.move_slash_selection(-SLASH_OVERLAY_PAGE_SIZE);
                return vec![];
            }
            KeyCode::PageDown => {
                state.move_slash_selection(SLASH_OVERLAY_PAGE_SIZE);
                return vec![];
            }
            KeyCode::Left => {
                state.cycle_selected_model_reasoning_effort(-1);
                return vec![];
            }
            KeyCode::Right => {
                state.cycle_selected_model_reasoning_effort(1);
                return vec![];
            }
            KeyCode::Enter => {
                let picked = state
                    .selected_model()
                    .map(|choice| choice.to_model_selection());
                state.slash_overlay = SlashOverlay::Hidden;
                let mut effects = vec![Effect::ClearTextarea];
                if let Some(model) = picked {
                    effects.push(Effect::SwitchModel(model));
                }
                return effects;
            }
            _ => {}
        }
    }

    match key_event.code {
        // Shift+Enter or Alt+Enter: insert a newline, exit history browsing.
        KeyCode::Enter
            if key_event.modifiers.contains(KeyModifiers::SHIFT)
                || key_event.modifiers.contains(KeyModifiers::ALT) =>
        {
            vec![Effect::ExitBrowsing, Effect::TextareaKey(key_event)]
        }

        KeyCode::Enter => {
            // Viewing a subagent's drill-in transcript: the child
            // conversation doesn't accept input (only one agent loop is
            // driven at a time), and silently steering the *root* agent
            // while a different (child) transcript is on screen would be
            // misleading — the user would have no visual confirmation of
            // which conversation their message actually went to. So Enter
            // is a no-op here until per-subagent steering exists (see
            // `docs/known-test-issues.md`).
            if matches!(state.active_view, ConversationView::Child { .. }) {
                return vec![];
            }
            let raw_text = &textarea.text;
            if raw_text.trim().is_empty() {
                return vec![];
            }
            // Parsed from the *raw* draft, not the dedented text: the
            // trigger is a `/` in the very first cell of the input, so an
            // indented line stays ordinary chat.
            if let Some(invocation) = parse_slash_command_invocation(raw_text) {
                state.slash_overlay = SlashOverlay::Hidden;
                return slash_command_effects(invocation);
            }
            let text = dedent(raw_text);
            let mode_is_idle = matches!(state.mode, TuiMode::Idle);
            if mode_is_idle {
                state.push_user_bubble(text.clone(), UserMessageState::Sent);
                state.mode = TuiMode::Pending;
            } else {
                // Stage rather than commit: chronologically-earlier
                // thinking/tool-call content for the in-flight round may not
                // be in `chat_entries` yet (see `queue_steer_message`).
                state.queue_steer_message(text.clone());
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
            let b = state.layout.clarify_banner;
            let in_clarify_banner =
                column >= b.x && column < b.x + b.width && row >= b.y && row < b.y + b.height;
            let hb = state.layout.header_bubble;
            let in_header_bubble =
                column >= hb.x && column < hb.x + hb.width && row >= hb.y && row < hb.y + hb.height;
            if matches!(state.focus, Focus::HelpPopup) {
                if scroll_up {
                    state.help_scroll_up(1);
                } else {
                    state.help_scroll_down(1);
                }
            } else if matches!(state.slash_overlay, SlashOverlay::ModelPicker { .. }) {
                state.move_slash_selection(if scroll_up { -1 } else { 1 });
            } else if in_clarify_banner {
                if scroll_up {
                    state.clarify_scroll_up(1);
                } else {
                    state.clarify_scroll_down(1);
                }
            } else if in_header_bubble {
                if scroll_up {
                    state.header_bubble_scroll_up(1);
                } else {
                    state.header_bubble_scroll_down(1);
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
                    state.restore_focus_after_help();
                    state.help_popup.help_scroll_offset = 0;
                    state.help_popup.help_scroll_x = 0;
                    state.clear_help_selection();
                } else if let Some((line, column)) = state.help_popup_content_position(column, row)
                {
                    state.help_popup.help_selection = Some(ChatSelection {
                        anchor_line: line,
                        anchor_col: column,
                        cursor_line: line,
                        cursor_col: column,
                    });
                    state.help_popup.help_drag_active = true;
                } else {
                    state.clear_help_selection();
                }
                return vec![];
            }

            // A click on a rendered slash-overlay row starts a possible text
            // drag. Model-picker clicks also change the highlight only;
            // confirmation remains an explicit Enter action.
            if state.slash_overlay.is_visible() {
                if let Some(overlay_row) = state.layout.slash_overlay.row_at(column, row) {
                    state.slash_overlay_drag_start = Some((column, row));
                    state.slash_overlay_drag_end = Some((column, row));
                    if let SlashOverlay::ModelPicker {
                        choices,
                        filter,
                        selected,
                    } = &mut state.slash_overlay
                    {
                        if let Some(entry_index) = model_picker_row_entries(choices, filter)
                            .get(overlay_row)
                            .copied()
                            .flatten()
                        {
                            *selected = entry_index;
                        }
                    }
                    return vec![];
                }
            }

            if let Some(offset) = state.layout.chat_scrollbar.offset_at(column, row) {
                state.conversation.scroll_offset = offset;
                state.conversation.scroll_locked =
                    offset == state.layout.chat_scrollbar.maximum_scroll_offset;
                return vec![];
            }

            // A header click is held until mouse-up so a drag can select and
            // copy its rendered text instead of toggling the bubble.
            let hb = state.layout.header_bubble;
            let in_header_bubble =
                column >= hb.x && column < hb.x + hb.width && row >= hb.y && row < hb.y + hb.height;
            if in_header_bubble {
                if let Some((anchor_line, anchor_col)) =
                    state.header_bubble_content_position(column, row)
                {
                    state.child_view.header_selection = Some(ChatSelection {
                        anchor_line,
                        anchor_col,
                        cursor_line: anchor_line,
                        cursor_col: anchor_col,
                    });
                    state.child_view.header_drag_active = true;
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
                    return vec![
                        Effect::ApproveTool { id: tool_id },
                        Effect::RestoreInputState,
                    ];
                }
                if in_deny {
                    state.focus = Focus::MainInput;
                    return vec![
                        Effect::DenyTool {
                            id: tool_id,
                            reason: String::new(),
                        },
                        Effect::RestoreInputState,
                    ];
                }
            }

            let in_chat = state.layout.chat_area.contains(column, row);
            let in_input = state.layout.input_area.contains(column, row);

            // Clarify banner: record mousedown for the whole banner area so the
            // user can drag-select any text (question or choices). The choice is
            // filled on mouseup only when the position matches (click, not drag).
            if matches!(state.mode, TuiMode::ClarifyPending { .. }) {
                let banner = state.layout.clarify_banner;
                if !banner.is_empty()
                    && column >= banner.x
                    && column < banner.x + banner.width
                    && row >= banner.y
                    && row < banner.y + banner.height
                {
                    state.clarify_banner_state.clarify_choice_mousedown = Some((column, row));
                    state.clarify_banner_state.clarify_banner_drag_end = None;
                    return vec![];
                }
            }

            if in_chat {
                let anchor_line = state
                    .conversation
                    .scroll_offset
                    .saturating_add((row.saturating_sub(state.layout.chat_area.rect.y)) as usize);
                let anchor_col = (column.saturating_sub(state.layout.chat_area.rect.x)) as usize;
                state.conversation.chat_selection = Some(ChatSelection {
                    anchor_line,
                    anchor_col,
                    cursor_line: anchor_line,
                    cursor_col: anchor_col,
                });
                state.conversation.chat_drag_active = true;
                vec![]
            } else if in_input {
                vec![Effect::PlaceCursorAtMousePos { column, row }]
            } else {
                vec![]
            }
        }

        MouseEventKind::Drag(crossterm::event::MouseButton::Left) => {
            if state.slash_overlay.is_visible() && state.slash_overlay_drag_start.is_some() {
                state.slash_overlay_drag_end = Some((column, row));
                return vec![];
            }
            if state.help_popup.help_drag_active {
                if let Some((line, column)) = state.help_popup_drag_position(column, row) {
                    if let Some(selection) = state.help_popup.help_selection.as_mut() {
                        selection.cursor_line = line;
                        selection.cursor_col = column;
                    }
                }
                return vec![];
            }
            if state.child_view.header_drag_active {
                if let Some((line, column)) = state.header_bubble_drag_position(column, row) {
                    if let Some(selection) = state.child_view.header_selection.as_mut() {
                        selection.cursor_line = line;
                        selection.cursor_col = column;
                    }
                }
                return vec![];
            }
            // Banner drag: update the highlight endpoint; suppress input selection.
            if state
                .clarify_banner_state
                .clarify_choice_mousedown
                .is_some()
            {
                state.clarify_banner_state.clarify_banner_drag_end = Some((column, row));
                return vec![];
            }
            if state.conversation.chat_drag_active {
                if let Some(sel) = state.conversation.chat_selection.as_mut() {
                    sel.cursor_line = state.conversation.scroll_offset.saturating_add(
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
            if !state.slash_overlay.is_visible() {
                state.slash_overlay_drag_start = None;
                state.slash_overlay_drag_end = None;
            }
            if let Some((start_column, start_row)) = state.slash_overlay_drag_start.take() {
                let (end_column, end_row) =
                    state.slash_overlay_drag_end.take().unwrap_or((column, row));
                let text =
                    extract_slash_overlay_text(state, start_column, start_row, end_column, end_row);
                if !text.is_empty() {
                    return vec![Effect::CopyToClipboard(text)];
                }
                return vec![];
            }
            // Clarify banner: click selects a choice; drag copies selected text.
            if let Some((down_col, down_row)) =
                state.clarify_banner_state.clarify_choice_mousedown.take()
            {
                let drag_end = state.clarify_banner_state.clarify_banner_drag_end.take();
                if let Some((up_col, up_row)) =
                    drag_end.or(if column == down_col && row == down_row {
                        None
                    } else {
                        Some((column, row))
                    })
                {
                    // Drag — extract the text of selected banner rows and copy.
                    let text =
                        extract_clarify_banner_text(state, down_row, down_col, up_row, up_col);
                    if !text.is_empty() {
                        return vec![Effect::CopyToClipboard(text)];
                    }
                    return vec![];
                }
                // Click — fill textarea with choice text if on a choice row.
                if let TuiMode::ClarifyPending {
                    ref question,
                    ref context,
                    choices: Some(ref choices_list),
                    ..
                } = state.mode.clone()
                {
                    let banner = state.layout.clarify_banner;
                    let wrap_width = banner.width as usize;
                    let context_rows = context
                        .as_deref()
                        .map_or(0, |ctx| render_markdown(ctx, wrap_width).len() as u16);
                    let context_spacer = u16::from(context.is_some());
                    let question_rows = render_markdown(question, wrap_width).len().max(1) as u16;
                    let choices_start_row = context_rows + context_spacer + question_rows;
                    let relative_row = row.saturating_sub(banner.y);
                    let content_row =
                        relative_row as usize + state.clarify_banner_state.clarify_scroll_offset;
                    if content_row >= choices_start_row as usize {
                        let choice_index = content_row - choices_start_row as usize;
                        if let Some(choice_text) = choices_list.get(choice_index) {
                            let rendered = format!("{}. {}", choice_index + 1, choice_text);
                            let text_end_col = banner.x + rendered.chars().count() as u16;
                            if column < text_end_col {
                                return vec![Effect::SetInputText(choice_text.clone())];
                            }
                        }
                    }
                }
                return vec![];
            }

            if state.help_popup.help_drag_active {
                state.help_popup.help_drag_active = false;
                if let Some(text) = state.extract_help_selection_text() {
                    if !text.is_empty() {
                        return vec![Effect::CopyToClipboard(text)];
                    }
                }
                return vec![];
            }

            if state.child_view.header_drag_active {
                state.child_view.header_drag_active = false;
                let selection = state.child_view.header_selection;
                if let Some(text) = state.extract_header_selection_text() {
                    if !text.is_empty() {
                        return vec![Effect::CopyToClipboard(text)];
                    }
                }
                if selection.is_some_and(|selection| !selection.is_empty()) {
                    return vec![];
                }
                state.child_view.header_selection = None;
                state.toggle_header_bubble_collapsed();
                return vec![];
            }

            if !state.conversation.chat_drag_active
                && state.layout.chat_scrollbar.offset_at(column, row).is_some()
            {
                return vec![];
            }

            if state.conversation.chat_drag_active {
                state.conversation.chat_drag_active = false;
                let selected_text = state.extract_chat_selection_text();
                if let Some(text) = selected_text {
                    if !text.is_empty() {
                        return vec![Effect::CopyToClipboard(text)];
                    }
                }
                // Plain click — toggle bubble if clicked on title row.
                if let Some(sel) = state.conversation.chat_selection {
                    let clicked_line = sel.anchor_line;
                    let clicked_col = sel.anchor_col;
                    if let Some((_, _, bubble_id)) = state
                        .conversation
                        .bubble_title_rows
                        .iter()
                        .find(|(start, end, _)| {
                            clicked_line == *start
                                || (clicked_col == 0
                                    && clicked_line >= *start
                                    && clicked_line <= *end)
                        })
                        .copied()
                    {
                        // A double-click on a `spawn_subagent` bubble's title
                        // drills into *that bubble's own* conversation view
                        // instead of toggling collapse — distinct from the
                        // single-click collapse/expand every bubble
                        // (including spawn bubbles) always supports.
                        // `run_id_for_bubble` still resolves a finished
                        // child's bubble (entries in `child_conversations`
                        // are kept indefinitely), and correctly disambiguates
                        // between multiple concurrent/queued `spawn_subagent`
                        // bubbles rather than assuming "the" single child.
                        //
                        // The first click on a spawn bubble does NOT toggle
                        // collapse immediately — it only arms
                        // `last_bubble_click`. Toggling immediately would
                        // change the bubble's rendered height and could shift
                        // a bottom-pinned auto-scroll before the second click
                        // arrives, breaking the row lookup that correlates it
                        // back to this bubble. Deferring the toggle until
                        // `BubbleClickWindowExpired` fires (see
                        // `bubble_click_timeout` in tui_driver.rs) keeps the
                        // layout stable across both clicks of a double-click.
                        let is_spawn_bubble = state.chat_entries.iter().any(|entry| {
                            matches!(entry, ChatEntry::ToolBubble { id, name, .. }
                                if *id == bubble_id && name == crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME)
                        });
                        if is_spawn_bubble {
                            let now = std::time::Instant::now();
                            let is_double_click = matches!(
                                state.last_bubble_click,
                                Some((last_id, clicked_at))
                                    if last_id == bubble_id
                                        && now.duration_since(clicked_at)
                                            < crate::tui_reducer_state::DOUBLE_CLICK_WINDOW
                            );
                            if is_double_click {
                                state.last_bubble_click = None;
                                if let Some(run_id) = state.run_id_for_bubble(bubble_id) {
                                    if state.child_needs_lazy_load(run_id) {
                                        return vec![Effect::LoadChildTranscript { run_id }];
                                    }
                                    state.switch_to_child_view(run_id);
                                } else {
                                    state.toggle_tool_bubble_collapsed(bubble_id);
                                }
                            } else {
                                state.last_bubble_click = Some((bubble_id, now));
                            }
                        } else {
                            state.toggle_tool_bubble_collapsed(bubble_id);
                        }
                    } else if let Some((_, _, entry_index)) = state
                        .conversation
                        .thinking_title_rows
                        .iter()
                        .find(|(start, end, _)| {
                            clicked_line == *start
                                || (clicked_col == 0
                                    && clicked_line >= *start
                                    && clicked_line <= *end)
                        })
                        .copied()
                    {
                        if entry_index == crate::tui_reducer_state::STREAMING_THINKING_ENTRY_INDEX {
                            state.toggle_partial_thinking_collapsed();
                        } else {
                            state.toggle_thinking_collapsed(entry_index);
                        }
                    }
                }
                state.conversation.chat_selection = None;
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
    match &event {
        DomainEvent::ChildAgentToken { run_id, .. }
        | DomainEvent::ChildAgentDone { run_id, .. } => state.revise_stream(Some(*run_id)),
        DomainEvent::StreamStarted
        | DomainEvent::StreamChunk(_)
        | DomainEvent::StreamThinkingChunk(_)
        | DomainEvent::StreamDone
        | DomainEvent::StreamCancelled => state.revise_stream(None),
        DomainEvent::CompactionApplied { run_id, .. } => {
            state.invalidate_transcript_layout(*run_id)
        }
        _ => {}
    }
    if matches!(event, DomainEvent::StreamCancelled) {
        state.compaction_requests.remove(&None);
    }
    let event_kind = event.kind();
    let correlation_id = event.correlation_id();
    let mode_before = state.mode.kind();
    // Child-lifecycle events never depend on / alter the parent's own
    // `TuiMode` (that's driven exclusively by the root's own stream/tool
    // state) — handle them uniformly ahead of the per-mode dispatch below.
    let effects = domain_child_lifecycle(state, event);
    // Deliberately logs only the event's variant tag, its correlation `id`
    // (when it has one), and the mode transition — never chunk text, tool
    // arguments, or tool output — just enough to follow the state-transition
    // flow and match up a call's `ToolStart`/`ToolProgress`/`ToolDone` (or
    // `ClarifyPending`) sequence across log lines.
    tracing::debug!(
        event = event_kind,
        correlation_id = ?correlation_id,
        mode_before,
        mode_after = state.mode.kind(),
        "domain event handled"
    );
    effects
}

/// Handles child lifecycle/configuration events ahead of the mode-specific
/// dispatch; every other event falls through unchanged to the per-`TuiMode`
/// handlers.
fn domain_child_lifecycle(state: &mut AppState, event: DomainEvent) -> Vec<Effect> {
    match event {
        DomainEvent::CompactionActivity {
            run_id,
            request_id,
            active,
        } => {
            if active {
                state.compaction_requests.insert(run_id, request_id);
            } else if state.compaction_requests.get(&run_id) == Some(&request_id) {
                state.compaction_requests.remove(&run_id);
            }
            vec![]
        }
        DomainEvent::ChildAgentStarted {
            run_id,
            call_id,
            agent_type,
            task,
            label,
        } => {
            state.start_child_run(ChildRunInfo {
                run_id,
                bubble_id: call_id,
                agent_type,
                task,
                label,
            });
            state.child_view.header_bubble_scroll_offset = 0;
            // No synthetic "task" message is seeded here: the child-view
            // header re-renders the real root `spawn_subagent` bubble (see
            // `render::project_for_draw`'s `child_view_bubble`), which
            // already shows the task/label/agent_type — duplicating it as
            // the transcript's first turn would just repeat the same text.
            vec![]
        }
        DomainEvent::ChildAgentEngineConfigured { run_id, engine } => {
            state.child_engine_displays.insert(run_id, engine);
            vec![]
        }
        DomainEvent::ChildAgentToken {
            run_id,
            text,
            is_thinking,
        } => {
            if let Some(conversation) = state.child_conversations.get_mut(&run_id) {
                if is_thinking {
                    conversation
                        .turn_buffer
                        .push_thinking_chunk(&text, &mut conversation.messages);
                } else {
                    conversation
                        .turn_buffer
                        .push_response_chunk(&text, &mut conversation.messages);
                }
            }
            vec![]
        }
        DomainEvent::ChildContextUsage {
            run_id,
            estimated_tokens,
            context_length,
            source,
        } => {
            if let Some(conversation) = state.child_conversations.get_mut(&run_id) {
                conversation.context_usage = Some(ContextUsage {
                    estimated_tokens,
                    context_length,
                    source,
                });
            }
            vec![]
        }
        DomainEvent::ChildAgentDone { run_id, .. } => {
            state.compaction_requests.remove(&Some(run_id));
            state.flush_child_partial_text(run_id);
            // Only auto-return if the user is currently viewing *this*
            // finished child's transcript — a different child may still be
            // running/queued, or the user may be looking at an unrelated
            // (earlier-finished) child's transcript, in which case this
            // event must not disturb `active_view`.
            let is_viewing_this_child = matches!(
                state.active_view,
                ConversationView::Child { run_id: viewed_run_id, .. } if viewed_run_id == run_id
            );
            if is_viewing_this_child && state.auto_switched_to_child {
                state.switch_to_root_view();
            }
            if state
                .running_child
                .as_ref()
                .is_some_and(|child| child.run_id == run_id)
            {
                state.running_child = None;
            }
            vec![]
        }
        DomainEvent::BubbleClickWindowExpired => {
            // Commit the deferred toggle from a single (non-double) click on
            // a spawn bubble's title — see the click handler in `update()`.
            // A no-op if a confirming second click already resolved (and
            // cleared) the arm first.
            if let Some((bubble_id, _)) = state.last_bubble_click.take() {
                state.toggle_tool_bubble_collapsed(bubble_id);
            }
            vec![]
        }
        DomainEvent::BubbleExpansionFeedbackShown => {
            state.pending_bubble_expansion = None;
            vec![]
        }
        DomainEvent::CompactionApplied {
            run_id,
            entry_index,
            summarized_messages,
        } => {
            let entries = match run_id {
                Some(run_id) => {
                    let Some(conversation) = state.child_conversations.get_mut(&run_id) else {
                        tracing::warn!(%run_id, "compaction marker has no owning child conversation");
                        return vec![];
                    };
                    &mut conversation.messages
                }
                None => &mut state.chat_entries,
            };
            // Mode-independent like the other transcript bookkeeping events:
            // a sweep applies compaction at a settled boundary, whatever the
            // view state happens to be. Duplicate emissions are dropped by
            // the boundary itself — it only ever grows.
            let already_marked = entries.iter().any(|entry| {
                matches!(
                    entry,
                    ChatEntry::CompactionMarker {
                        summarized_messages: existing
                    } if *existing == summarized_messages
                )
            });
            if !already_marked {
                let insert_at = entry_index.min(entries.len());
                entries.insert(
                    insert_at,
                    ChatEntry::CompactionMarker {
                        summarized_messages,
                    },
                );
            }
            vec![]
        }
        DomainEvent::ContextUsage {
            estimated_tokens,
            context_length,
            source,
        } => {
            // Mode-independent like the child-lifecycle events: a settled
            // turn reports the readout while the loop is idle, and a summary
            // outcome can do so without touching the mode.
            state.context_usage = Some(ContextUsage {
                estimated_tokens,
                context_length,
                source,
            });
            vec![]
        }
        other => match state.mode.clone() {
            TuiMode::Idle => domain_idle(state, other),
            TuiMode::Pending => domain_pending(state, other),
            TuiMode::Streaming => domain_streaming(state, other),
            TuiMode::ClarifyPending { .. } => domain_clarify_pending(state, other),
            TuiMode::Aborting => domain_aborting(state, other),
            TuiMode::DoublePressAbort {
                pressed_at,
                return_to,
                armed_by,
            } => domain_double_press(state, other, pressed_at, return_to, armed_by),
        },
    }
}

/// Shared handling for `DomainEvent::ToolPending`, used identically by
/// `domain_idle`, `domain_pending`, and `domain_streaming`: creates the
/// `Pending` bubble in whichever transcript `run_id` targets (root or a
/// child), arms `Focus::ToolApproval`, and always auto-switches the drill-in
/// view to wherever this pending call actually lives — root or that
/// specific child — regardless of which conversation is currently on
/// screen (see `AppState::note_interactive_pause`).
fn push_tool_pending_bubble(
    state: &mut AppState,
    id: Uuid,
    name: String,
    arguments: serde_json::Value,
    content_sections: Vec<crate::agent::tools::Section>,
    title_arguments: Vec<(String, serde_json::Value)>,
    run_id: Option<Uuid>,
) -> Vec<Effect> {
    state.flush_target_partial_text(run_id);
    state
        .target_messages_mut(run_id)
        .push(ChatEntry::ToolBubble {
            id,
            name: name.clone(),
            state: Box::new(ToolCallState::Pending {
                arguments,
                content_sections,
                title_arguments,
            }),
            collapsed: true,
        });
    state.focus = Focus::ToolApproval {
        tool_id: id,
        tool_name: name,
    };
    // The approval footer occupies the rows directly above the input, the
    // same rows any slash overlay draws itself onto, and the approval also
    // takes over keyboard routing — so the save effect below parks the open
    // overlay (notably a `/models` picker's already-fetched catalog, which
    // the draft alone cannot reproduce) into `pending_input_snapshot` and
    // hides it until `RestoreInputState` brings draft and overlay back
    // together when the approval resolves.
    state.note_interactive_pause(run_id);
    vec![Effect::SaveInputStateAndClear]
}

/// Shared handling for `DomainEvent::ClarifyPending`, used identically by
/// `domain_idle`, `domain_pending`, and `domain_streaming`. See
/// `push_tool_pending_bubble` for the `run_id` routing/auto-switch rationale.
fn push_clarify_pending_bubble(
    state: &mut AppState,
    id: Uuid,
    question: String,
    context: Option<String>,
    choices: Option<Vec<String>>,
    run_id: Option<Uuid>,
) -> Vec<Effect> {
    let mut bubble_arguments = serde_json::json!({ "question": question });
    if let Some(ref ctx) = context {
        bubble_arguments["context"] = serde_json::Value::String(ctx.clone());
    }
    if let Some(ref choices_list) = choices {
        bubble_arguments["choices"] = serde_json::json!(choices_list);
    }
    // Flush any thinking/assistant text accumulated before this clarify call
    // so it lands ahead of the clarify bubble in chronological order (mirrors
    // the ToolStart/ToolPending flush).
    state.flush_target_partial_text(run_id);
    state
        .target_messages_mut(run_id)
        .push(ChatEntry::ToolBubble {
            id,
            name: ClarifyTool::NAME.to_string(),
            state: Box::new(ToolCallState::Running {
                arguments: bubble_arguments,
                content_sections: vec![],
                title_arguments: vec![(
                    "question".to_string(),
                    serde_json::Value::String(question.clone()),
                )],
                progress_arguments: vec![],
            }),
            collapsed: false,
        });
    state.mode = TuiMode::ClarifyPending {
        id,
        question,
        context,
        choices,
    };
    // Scroll the clarify banner to the bottom so the question and choices
    // are visible immediately; clamped to max_scroll on render.
    state.clarify_banner_state.clarify_scroll_offset = usize::MAX;
    state.focus = Focus::MainInput;
    state.note_interactive_pause(run_id);
    // The banner sits on the same rows above the input as the approval
    // footer, so this save parks draft *and* overlay together — see
    // `InputSnapshot`.
    vec![Effect::SaveInputStateAndClear]
}

fn domain_idle(state: &mut AppState, event: DomainEvent) -> Vec<Effect> {
    match event {
        DomainEvent::StreamStarted => {
            state.flush_partial_assistant_message(false);
            state.mode = TuiMode::Streaming;
            state.turn_buffer.partial_assistant_text.clear();
            state.turn_buffer.partial_thinking_text.clear();
            vec![]
        }
        DomainEvent::ToolPending {
            id,
            name,
            arguments,
            content_sections,
            title_arguments,
            run_id,
        } => push_tool_pending_bubble(
            state,
            id,
            name,
            arguments,
            content_sections,
            title_arguments,
            run_id,
        ),
        DomainEvent::ClarifyPending {
            id,
            question,
            context,
            choices,
            run_id,
        } => push_clarify_pending_bubble(state, id, question, context, choices, run_id),
        DomainEvent::Error { message, run_id } => {
            if run_id.is_none() {
                state.mode = TuiMode::Idle;
            }
            state
                .target_messages_mut(run_id)
                .push(ChatEntry::ErrorLine { message });
            vec![]
        }
        DomainEvent::SteerQueued { run_id } => {
            // No driver routes `Prompt` to a child today (see
            // `UiUpdate::SteerQueued::run_id`), so `run_id: Some(_)` is
            // unreachable in practice; guard anyway rather than silently
            // flipping the root's own indicator for a child steer.
            if run_id.is_none() {
                state.steer_indicator = true;
            }
            vec![]
        }
        DomainEvent::SteerApplied { run_id } => {
            // See `DomainEvent::SteerQueued` above: only the root's own
            // steer queue/indicator is meaningful today.
            if run_id.is_none() {
                state.commit_queued_steer_messages(UserMessageState::Steered);
                state.steer_indicator = false;
            }
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
            run_id,
        } => {
            update_tool_bubble_to_running(
                state,
                id,
                name,
                arguments,
                content_sections,
                title_arguments,
                run_id,
            );
            vec![]
        }
        DomainEvent::ToolProgress {
            id,
            title_arguments,
        } => {
            update_tool_bubble_progress(state, id, title_arguments);
            vec![]
        }
        DomainEvent::ToolDone {
            id,
            name,
            result_section,
            summary,
            duration,
            content_sections,
            run_id,
        } => {
            update_tool_bubble_to_done(
                state,
                ToolDonePayload {
                    id,
                    name,
                    result_section,
                    content_sections,
                    summary,
                    duration,
                    run_id,
                },
            );
            vec![]
        }
        DomainEvent::ToolKilled { id, run_id } => {
            update_tool_bubble_to_killed(state, id, run_id);
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
            state.turn_buffer.partial_assistant_text.clear();
            state.turn_buffer.partial_thinking_text.clear();
            vec![]
        }
        DomainEvent::StreamDone => {
            state.flush_partial_assistant_message(false);
            state.mode = TuiMode::Idle;
            vec![]
        }
        DomainEvent::StreamCancelled => {
            state.flush_partial_assistant_message(true);
            state.commit_queued_steer_messages(UserMessageState::Cancelled);
            state.steer_indicator = false;
            state.mode = TuiMode::Idle;
            vec![]
        }
        DomainEvent::Error { message, run_id } => {
            if run_id.is_none() {
                state.mode = TuiMode::Idle;
            }
            state
                .target_messages_mut(run_id)
                .push(ChatEntry::ErrorLine { message });
            vec![]
        }
        DomainEvent::ToolPending {
            id,
            name,
            arguments,
            content_sections,
            title_arguments,
            run_id,
        } => push_tool_pending_bubble(
            state,
            id,
            name,
            arguments,
            content_sections,
            title_arguments,
            run_id,
        ),
        DomainEvent::ClarifyPending {
            id,
            question,
            context,
            choices,
            run_id,
        } => push_clarify_pending_bubble(state, id, question, context, choices, run_id),
        DomainEvent::SteerQueued { run_id } => {
            // No driver routes `Prompt` to a child today (see
            // `UiUpdate::SteerQueued::run_id`), so `run_id: Some(_)` is
            // unreachable in practice; guard anyway rather than silently
            // flipping the root's own indicator for a child steer.
            if run_id.is_none() {
                state.steer_indicator = true;
            }
            vec![]
        }
        DomainEvent::SteerApplied { run_id } => {
            // See `DomainEvent::SteerQueued` above: only the root's own
            // steer queue/indicator is meaningful today.
            if run_id.is_none() {
                state.commit_queued_steer_messages(UserMessageState::Steered);
                state.steer_indicator = false;
            }
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
            run_id,
        } => {
            update_tool_bubble_to_running(
                state,
                id,
                name,
                arguments,
                content_sections,
                title_arguments,
                run_id,
            );
            vec![]
        }
        DomainEvent::ToolProgress {
            id,
            title_arguments,
        } => {
            update_tool_bubble_progress(state, id, title_arguments);
            vec![]
        }
        DomainEvent::ToolDone {
            id,
            name,
            result_section,
            summary,
            duration,
            content_sections,
            run_id,
        } => {
            update_tool_bubble_to_done(
                state,
                ToolDonePayload {
                    id,
                    name,
                    result_section,
                    content_sections,
                    summary,
                    duration,
                    run_id,
                },
            );
            vec![]
        }
        DomainEvent::ToolKilled { id, run_id } => {
            update_tool_bubble_to_killed(state, id, run_id);
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
            // A response chunk following pending thinking text means the
            // model just finished a thinking segment and switched to reply
            // text — `push_response_chunk` flushes that segment into its own
            // bubble first, rather than merging it with whatever thinking
            // segment follows later in the same turn.
            state
                .turn_buffer
                .push_response_chunk(&text, &mut state.chat_entries);
            if state.conversation.scroll_locked {
                state.conversation.scroll_offset = 0;
            }
            vec![]
        }
        DomainEvent::StreamThinkingChunk(text) => {
            // Mirrors the flush above: `push_thinking_chunk` flushes any
            // pending reply segment so far into its own bubble when the
            // model switches back to thinking.
            state
                .turn_buffer
                .push_thinking_chunk(&text, &mut state.chat_entries);
            if state.conversation.scroll_locked {
                state.conversation.scroll_offset = 0;
            }
            vec![]
        }
        DomainEvent::StreamDone => {
            state.flush_partial_assistant_message(false);
            state.mode = TuiMode::Idle;
            vec![]
        }
        DomainEvent::StreamCancelled => {
            state.flush_partial_assistant_message(true);
            state.commit_queued_steer_messages(UserMessageState::Cancelled);
            state.steer_indicator = false;
            if matches!(state.focus, Focus::ToolApproval { .. }) {
                state.focus = Focus::MainInput;
            }
            state.mode = TuiMode::Idle;
            vec![]
        }
        DomainEvent::ToolPending {
            id,
            name,
            arguments,
            content_sections,
            title_arguments,
            run_id,
        } => push_tool_pending_bubble(
            state,
            id,
            name,
            arguments,
            content_sections,
            title_arguments,
            run_id,
        ),
        DomainEvent::ClarifyPending {
            id,
            question,
            context,
            choices,
            run_id,
        } => push_clarify_pending_bubble(state, id, question, context, choices, run_id),
        DomainEvent::ToolStart {
            id,
            name,
            arguments,
            content_sections,
            title_arguments,
            run_id,
        } => {
            update_tool_bubble_to_running(
                state,
                id,
                name,
                arguments,
                content_sections,
                title_arguments,
                run_id,
            );
            vec![]
        }
        DomainEvent::ToolProgress {
            id,
            title_arguments,
        } => {
            update_tool_bubble_progress(state, id, title_arguments);
            vec![]
        }
        DomainEvent::ToolDone {
            id,
            name,
            result_section,
            summary,
            duration,
            content_sections,
            run_id,
        } => {
            update_tool_bubble_to_done(
                state,
                ToolDonePayload {
                    id,
                    name,
                    result_section,
                    content_sections,
                    summary,
                    duration,
                    run_id,
                },
            );
            vec![]
        }
        DomainEvent::ToolKilled { id, run_id } => {
            update_tool_bubble_to_killed(state, id, run_id);
            vec![]
        }
        DomainEvent::ToolDenied { id } => {
            update_tool_bubble_to_denied(state, id);
            if matches!(state.focus, Focus::ToolApproval { tool_id, .. } if tool_id == id) {
                state.focus = Focus::MainInput;
            }
            vec![]
        }
        DomainEvent::Error { message, run_id } => {
            if run_id.is_none() {
                state.mode = TuiMode::Idle;
            }
            state
                .target_messages_mut(run_id)
                .push(ChatEntry::ErrorLine { message });
            vec![]
        }
        DomainEvent::SteerQueued { run_id } => {
            // No driver routes `Prompt` to a child today (see
            // `UiUpdate::SteerQueued::run_id`), so `run_id: Some(_)` is
            // unreachable in practice; guard anyway rather than silently
            // flipping the root's own indicator for a child steer.
            if run_id.is_none() {
                state.steer_indicator = true;
            }
            vec![]
        }
        DomainEvent::SteerApplied { run_id } => {
            // See `DomainEvent::SteerQueued` above: only the root's own
            // steer queue/indicator is meaningful today.
            if run_id.is_none() {
                state.commit_queued_steer_messages(UserMessageState::Steered);
                state.steer_indicator = false;
            }
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

fn domain_clarify_pending(state: &mut AppState, event: DomainEvent) -> Vec<Effect> {
    match event {
        DomainEvent::StreamStarted => {
            state.flush_partial_assistant_message(false);
            state.mode = TuiMode::Streaming;
            state.turn_buffer.partial_assistant_text.clear();
            state.turn_buffer.partial_thinking_text.clear();
            vec![]
        }
        DomainEvent::ToolDone {
            id,
            name,
            result_section,
            summary,
            duration,
            content_sections,
            run_id,
        } => {
            update_tool_bubble_to_done(
                state,
                ToolDonePayload {
                    id,
                    name,
                    result_section,
                    content_sections,
                    summary,
                    duration,
                    run_id,
                },
            );
            state.mode = TuiMode::Pending;
            vec![]
        }
        DomainEvent::AgentIdle | DomainEvent::StreamCancelled => {
            state.mode = TuiMode::Idle;
            state.focus = Focus::MainInput;
            vec![]
        }
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
            state.commit_queued_steer_messages(UserMessageState::Cancelled);
            state.steer_indicator = false;
            if matches!(state.focus, Focus::ToolApproval { .. }) {
                state.focus = Focus::MainInput;
            }
            state.mode = TuiMode::Idle;
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
            vec![Effect::DenyTool {
                id,
                reason: String::new(),
            }]
        }
        DomainEvent::ToolStart {
            id,
            name,
            arguments,
            content_sections,
            title_arguments,
            run_id,
        } => {
            update_tool_bubble_to_running(
                state,
                id,
                name,
                arguments,
                content_sections,
                title_arguments,
                run_id,
            );
            vec![]
        }
        DomainEvent::ToolProgress {
            id,
            title_arguments,
        } => {
            update_tool_bubble_progress(state, id, title_arguments);
            vec![]
        }
        DomainEvent::ToolDone {
            id,
            name,
            result_section,
            summary,
            duration,
            content_sections,
            run_id,
        } => {
            update_tool_bubble_to_done(
                state,
                ToolDonePayload {
                    id,
                    name,
                    result_section,
                    content_sections,
                    summary,
                    duration,
                    run_id,
                },
            );
            vec![]
        }
        DomainEvent::ToolKilled { id, run_id } => {
            tracing::info!(%id, aborting_exit = state.aborting_exit, "aborting: tool killed → idle");
            update_tool_bubble_to_killed(state, id, run_id);
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
        DomainEvent::Error { message, run_id } => {
            if run_id.is_none() {
                state.mode = TuiMode::Idle;
            }
            state
                .target_messages_mut(run_id)
                .push(ChatEntry::ErrorLine { message });
            vec![]
        }
        DomainEvent::StreamDone => {
            state.flush_partial_assistant_message(false);
            state.mode = TuiMode::Idle;
            vec![]
        }
        DomainEvent::StreamChunk(text) => {
            state
                .turn_buffer
                .push_response_chunk(&text, &mut state.chat_entries);
            vec![]
        }
        DomainEvent::StreamThinkingChunk(text) => {
            state
                .turn_buffer
                .push_thinking_chunk(&text, &mut state.chat_entries);
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
    armed_by: ConfirmKey,
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
                    armed_by,
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
    content_sections: Vec<crate::agent::tools::Section>,
    title_arguments: Vec<(String, serde_json::Value)>,
    run_id: Option<Uuid>,
) {
    if let Some(entry) = state.find_tool_bubble_mut(id) {
        if let ChatEntry::ToolBubble {
            state: bubble_state,
            ..
        } = entry
        {
            **bubble_state = ToolCallState::Running {
                arguments: arguments.clone(),
                content_sections: content_sections.clone(),
                title_arguments: title_arguments.clone(),
                progress_arguments: vec![],
            };
        }
    } else {
        // First appearance of this tool call (auto-run tool with no approval
        // step): flush any thinking/assistant text accumulated *before* this
        // call so it lands as its own bubble ahead of the tool bubble in
        // `chat_entries`, matching actual chronological order. Without this,
        // thinking that happens both before and after the tool call would
        // stay merged into one bubble and get flushed only at `StreamDone`,
        // making it look like the tool ran before the model ever "thought".
        state.flush_target_partial_text(run_id);
        state
            .target_messages_mut(run_id)
            .push(ChatEntry::ToolBubble {
                id,
                name,
                state: Box::new(ToolCallState::Running {
                    arguments,
                    content_sections,
                    title_arguments,
                    progress_arguments: vec![],
                }),
                collapsed: true,
            });
    }
    // Clear ToolApproval focus if it was for this tool.
    if matches!(state.focus, Focus::ToolApproval { tool_id, .. } if tool_id == id) {
        state.focus = Focus::MainInput;
    }
    // A tool moving from Pending to Running means its approval step (if any)
    // is over — if this was the child's interactive pause we auto-switched
    // for, auto-return to Root now that the child is unblocked again.
    state.resolve_possible_interactive_pause(id);
}

/// Patches `progress_arguments` on an existing `Running` tool bubble without
/// otherwise changing its state — used for interim status updates (e.g.
/// `spawn_subagent` reporting `status=spawned`/`queued` + `run_id`) that
/// don't mean the call itself is done. Deliberately separate from
/// `title_arguments` (the call's real, static arguments): `Done` has no
/// `progress_arguments` field, so these interim values are naturally
/// dropped once the call truly finishes instead of lingering stale on the
/// terminal bubble (see `ToolCallState::Running::progress_arguments`). A
/// no-op if the bubble isn't found or isn't currently `Running` (e.g. it
/// already reached a terminal state).
fn update_tool_bubble_progress(
    state: &mut AppState,
    id: Uuid,
    title_arguments: Vec<(String, serde_json::Value)>,
) {
    let new_multiline_content = title_arguments
        .iter()
        .any(|(_, value)| value.as_str().is_some_and(|value| value.contains('\n')));
    let mut body_changed = new_multiline_content;
    if let Some(ChatEntry::ToolBubble {
        state: bubble_state,
        ..
    }) = state.find_tool_bubble_title_mut(id)
    {
        if let ToolCallState::Running {
            progress_arguments, ..
        } = bubble_state.as_mut()
        {
            body_changed |= progress_arguments
                .iter()
                .any(|(_, value)| value.as_str().is_some_and(|value| value.contains('\n')));
            *progress_arguments = title_arguments;
        }
    }
    if body_changed {
        // Multiline title values also supply expandable content sections.
        let _ = state.find_tool_bubble_mut(id);
    }
}

/// Payload for [`update_tool_bubble_to_done`], grouped into a struct to keep
/// the function signature down to a reasonable arity (see
/// `DomainEvent::ToolDone`, which this mirrors field-for-field plus `id`).
struct ToolDonePayload {
    id: Uuid,
    name: String,
    result_section: crate::tui_reducer_state::ResultSection,
    content_sections: Vec<crate::agent::tools::Section>,
    summary: Vec<(String, String)>,
    duration: std::time::Duration,
    run_id: Option<Uuid>,
}

fn update_tool_bubble_to_done(state: &mut AppState, payload: ToolDonePayload) {
    let ToolDonePayload {
        id,
        name,
        result_section,
        content_sections,
        summary,
        duration,
        run_id,
    } = payload;
    let (previous_arguments, previous_title_arguments) = state
        .find_tool_bubble_mut(id)
        .and_then(|entry| {
            if let ChatEntry::ToolBubble {
                state: bubble_state,
                ..
            } = entry
            {
                match bubble_state.as_ref() {
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
            **bubble_state = ToolCallState::Done {
                arguments: previous_arguments,
                result_section,
                content_sections,
                summary,
                duration,
                title_arguments: previous_title_arguments,
            };
        }
    } else {
        state
            .target_messages_mut(run_id)
            .push(ChatEntry::ToolBubble {
                id,
                name,
                state: Box::new(ToolCallState::Done {
                    arguments: serde_json::Value::Null,
                    result_section,
                    content_sections,
                    summary,
                    duration,
                    title_arguments: vec![],
                }),
                collapsed: true,
            });
    }
    state.resolve_possible_interactive_pause(id);
}

fn update_tool_bubble_to_killed(state: &mut AppState, id: Uuid, run_id: Option<Uuid>) {
    if let Some(entry) = state.find_tool_bubble_mut(id) {
        if let ChatEntry::ToolBubble {
            state: bubble_state,
            ..
        } = entry
        {
            **bubble_state = ToolCallState::Killed;
        }
    } else {
        state
            .target_messages_mut(run_id)
            .push(ChatEntry::ToolBubble {
                id,
                name: String::new(),
                state: Box::new(ToolCallState::Killed),
                collapsed: true,
            });
    }
    if matches!(state.focus, Focus::ToolApproval { tool_id, .. } if tool_id == id) {
        state.focus = Focus::MainInput;
    }
    state.resolve_possible_interactive_pause(id);
}

fn update_tool_bubble_to_denied(state: &mut AppState, id: Uuid) {
    if let Some(ChatEntry::ToolBubble {
        state: bubble_state,
        ..
    }) = state.find_tool_bubble_mut(id)
    {
        let (preserved_arguments, preserved_content_sections, preserved_title_arguments) =
            match bubble_state.as_ref() {
                ToolCallState::Pending {
                    arguments,
                    content_sections,
                    title_arguments,
                }
                | ToolCallState::Running {
                    arguments,
                    content_sections,
                    title_arguments,
                    ..
                } => (
                    arguments.clone(),
                    content_sections.clone(),
                    title_arguments.clone(),
                ),
                _ => (serde_json::Value::Null, vec![], vec![]),
            };
        **bubble_state = ToolCallState::Denied {
            arguments: preserved_arguments,
            error: "Denied by user".to_string(),
            content_sections: preserved_content_sections,
            title_arguments: preserved_title_arguments,
        };
    }
    state.resolve_possible_interactive_pause(id);
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
    use std::time::{Duration, Instant};

    use crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use uuid::Uuid;

    use super::*;
    use crate::agent::tools::{
        ContentSection, DiffSection, SearchResultLine, SearchResultSection, Section,
    };
    use crate::agent::{AssistantStatus, Message, ToolCallRequest, ToolResult};
    use crate::tui_reducer_state::{
        AppState, ChatEntry, ChatSelection, ContextUsageSource, ConversationView, Focus,
        InputEvent, ModelChoice, ResultSection, SlashOverlay, ToolCallState, TuiMode,
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

    struct ToolBubbleRoundtripCase {
        tool_id: Uuid,
        tool_name: String,
        arguments: serde_json::Value,
        initial_content_sections: Vec<Section>,
        title_arguments: Vec<(String, serde_json::Value)>,
        result: String,
        result_metadata: Option<serde_json::Value>,
        result_metadata_schema: Option<serde_json::Value>,
        result_section: ResultSection,
        summary: Vec<(String, String)>,
        content_sections: Vec<Section>,
        duration: Duration,
    }

    struct ToolErrorBubbleRoundtripCase {
        tool_id: Uuid,
        tool_name: String,
        arguments: serde_json::Value,
        initial_content_sections: Vec<Section>,
        title_arguments: Vec<(String, serde_json::Value)>,
        error: String,
        duration: Duration,
        content_sections: Vec<Section>,
    }

    fn render_app_state_buffer(state: &mut AppState) -> Buffer {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let mut textarea = TextArea::default();
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

    fn assert_tool_bubble_roundtrip(case: ToolBubbleRoundtripCase) {
        assert_tool_bubble_roundtrip_with_child_runs(case, &[]);
    }

    fn assert_tool_bubble_roundtrip_with_child_runs(
        case: ToolBubbleRoundtripCase,
        child_runs: &[crate::store::ChildRunRecord],
    ) {
        let persisted_content_sections = if child_runs.is_empty() {
            case.content_sections.clone()
        } else {
            case.initial_content_sections.clone()
        };
        let mut live_state = AppState::new();
        live_state.mode = TuiMode::Streaming;
        update(
            &mut live_state,
            TuiEvent::Domain(DomainEvent::ToolStart {
                id: case.tool_id,
                name: case.tool_name.clone(),
                arguments: case.arguments.clone(),
                content_sections: case.initial_content_sections.clone(),
                title_arguments: case.title_arguments.clone(),
                run_id: None,
            }),
            &empty_textarea(),
        );
        update(
            &mut live_state,
            TuiEvent::Domain(DomainEvent::ToolDone {
                id: case.tool_id,
                name: case.tool_name.clone(),
                result_section: case.result_section.clone(),
                summary: case.summary.clone(),
                duration: case.duration,
                content_sections: case.content_sections.clone(),
                run_id: None,
            }),
            &empty_textarea(),
        );

        let persisted_messages = [
            Message::Assistant {
                content: String::new(),
                status: AssistantStatus::Complete,
                tool_calls: vec![ToolCallRequest {
                    id: case.tool_id,
                    name: case.tool_name.clone(),
                    arguments: case.arguments.clone(),
                    persisted_title_arguments: case.title_arguments.clone(),
                }],
            },
            Message::ToolResult {
                value: ToolResult {
                    id: case.tool_id,
                    name: case.tool_name.clone(),
                    result: case.result.clone(),
                    attachments: vec![],
                    result_section_id: Some(case.result_section.id.clone()),
                    result_metadata: case.result_metadata.clone(),
                    result_metadata_schema: case.result_metadata_schema.clone(),
                    summary: case.summary.clone(),
                    content_sections: persisted_content_sections,
                    duration: case.duration,
                },
            },
        ];
        let replayed_messages = persisted_messages
            .iter()
            .map(|message| {
                let serialized = serde_json::to_string(message).expect("message serializes");
                serde_json::from_str::<Message>(&serialized).expect("message deserializes")
            })
            .collect::<Vec<_>>();
        let mut replayed_state = AppState::new();
        replayed_state.replay_messages(&replayed_messages, child_runs, None);
        let replayed_entries = replayed_state.chat_entries.clone();

        // Keep the surrounding frame identical so this compares the actual
        // tool-bubble rendering rather than unrelated live/resumed mode chrome.
        live_state.mode = TuiMode::Idle;
        replayed_state.mode = TuiMode::Idle;
        let live_buffer = render_app_state_buffer(&mut live_state);
        let replayed_buffer = render_app_state_buffer(&mut replayed_state);

        assert_eq!(
            live_state.chat_entries, replayed_entries,
            "{} tool bubble must have the same reducer state before and after resume",
            case.tool_name
        );

        assert_eq!(
            live_buffer, replayed_buffer,
            "{} tool bubble must render identically before and after resume",
            case.tool_name
        );
    }

    fn assert_tool_error_bubble_roundtrip(case: ToolErrorBubbleRoundtripCase) {
        let mut live_state = AppState::new();
        live_state.mode = TuiMode::Streaming;
        update(
            &mut live_state,
            TuiEvent::Domain(DomainEvent::ToolStart {
                id: case.tool_id,
                name: case.tool_name.clone(),
                arguments: case.arguments.clone(),
                content_sections: case.initial_content_sections,
                title_arguments: case.title_arguments.clone(),
                run_id: None,
            }),
            &empty_textarea(),
        );
        live_state.apply_tool_error(
            case.tool_id,
            case.error.clone(),
            case.duration,
            case.content_sections.clone(),
            None,
        );

        let persisted_messages = [
            Message::Assistant {
                content: String::new(),
                status: AssistantStatus::Complete,
                tool_calls: vec![ToolCallRequest {
                    id: case.tool_id,
                    name: case.tool_name.clone(),
                    arguments: case.arguments,
                    persisted_title_arguments: case.title_arguments,
                }],
            },
            Message::ToolError {
                id: case.tool_id,
                name: case.tool_name,
                error: case.error,
                duration: case.duration,
                content_sections: case.content_sections,
            },
        ];
        let replayed_messages = persisted_messages
            .iter()
            .map(|message| {
                let serialized = serde_json::to_string(message).expect("message serializes");
                serde_json::from_str::<Message>(&serialized).expect("message deserializes")
            })
            .collect::<Vec<_>>();
        let mut replayed_state = AppState::new();
        replayed_state.replay_messages(&replayed_messages, &[], None);
        let replayed_entries = replayed_state.chat_entries.clone();

        live_state.mode = TuiMode::Idle;
        replayed_state.mode = TuiMode::Idle;
        let live_buffer = render_app_state_buffer(&mut live_state);
        let replayed_buffer = render_app_state_buffer(&mut replayed_state);

        assert_eq!(
            live_state.chat_entries, replayed_entries,
            "error bubble must have the same reducer state before and after resume"
        );
        assert_eq!(
            live_buffer, replayed_buffer,
            "error bubble must render identically before and after resume"
        );
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

    #[test]
    fn route_domain_logs_event_kind_and_mode_transition_without_chunk_text() {
        let log = crate::test_support::capture_logs(async {
            prime_domain_logging();
            let mut state = AppState::new();
            state.mode = TuiMode::Streaming;
            update(
                &mut state,
                TuiEvent::Domain(DomainEvent::StreamChunk(
                    "super-secret-assistant-text".to_string(),
                )),
                &empty_textarea(),
            );
        });
        assert!(
            log.contains("domain event handled"),
            "route_domain must log a state-transition event: {log}"
        );
        assert!(
            log.contains("event=\"StreamChunk\""),
            "the event's variant tag must be logged: {log}"
        );
        assert!(
            log.contains("mode_before=\"Streaming\"") && log.contains("mode_after=\"Streaming\""),
            "the mode transition must be logged: {log}"
        );
        assert!(
            !log.contains("super-secret-assistant-text"),
            "chunk payload text must never be logged: {log}"
        );
    }

    #[test]
    fn route_domain_logs_correlation_id_for_tool_events() {
        let id = Uuid::new_v4();
        let log = crate::test_support::capture_logs(async {
            prime_domain_logging();
            let mut state = AppState::new();
            state.mode = TuiMode::Streaming;
            update(
                &mut state,
                TuiEvent::Domain(DomainEvent::ToolStart {
                    id,
                    name: "read".to_string(),
                    arguments: serde_json::json!({"path": "/secret/path"}),
                    content_sections: vec![],
                    title_arguments: vec![],
                    run_id: None,
                }),
                &empty_textarea(),
            );
        });
        assert!(
            log.contains(&format!("correlation_id=Some({id})")),
            "the tool call's id must be logged for correlation: {log}"
        );
        assert!(
            !log.contains("/secret/path"),
            "tool arguments must never be logged: {log}"
        );
    }

    fn key_event(code: KeyCode) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn model_picker_pages_selection_with_page_keys() {
        let mut state = AppState::new();
        state.slash_overlay = SlashOverlay::ModelPicker {
            choices: (0..10)
                .map(|index| ModelChoice {
                    label: format!("stub/model-{index}"),
                    initial_reasoning_effort: None,
                    selection: ProviderModelOption {
                        provider_connection: "stub".to_string(),
                        model: crate::llm::ModelInfo {
                            id: format!("model-{index}"),
                            context_length: None,
                            reasoning_efforts: None,
                            reasoning_default: None,
                            input_modalities: None,
                        },
                        reasoning_effort: None,
                    },
                    is_active: false,
                    engine_name: None,
                    selection_target: None,
                    selection_preview_error: None,
                })
                .collect(),
            filter: String::new(),
            selected: 5,
        };
        let textarea = empty_textarea();

        update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::PageUp))),
            &textarea,
        );
        assert_eq!(
            state
                .selected_model()
                .map(|choice| choice.selection.model.id.as_str()),
            Some("model-2")
        );

        update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::PageDown))),
            &textarea,
        );
        assert_eq!(
            state
                .selected_model()
                .map(|choice| choice.selection.model.id.as_str()),
            Some("model-5")
        );
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

    #[test]
    fn child_agent_started_does_not_auto_switch_the_active_view() {
        // Spawning a child must not yank focus away from whatever the user
        // was looking at — only an *interactive pause* (ToolPending /
        // ClarifyPending) belonging to the child does that.
        let mut state = AppState::new();
        let run_id = Uuid::new_v4();
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ChildAgentStarted {
                run_id,
                call_id: Uuid::new_v4(),
                agent_type: "reviewer".to_string(),
                task: "look into flaky test".to_string(),
                label: "Investigate flaky test".to_string(),
            }),
            &empty_textarea(),
        );
        assert_eq!(state.active_view, ConversationView::Root);
        assert!(!state.auto_switched_to_child);
    }

    #[test]
    fn child_agent_started_clears_transcript_without_seeding_a_synthetic_task_message() {
        // The task is shown via the real root `spawn_subagent` bubble
        // re-rendered as the child-view header (see
        // `render::project_for_draw`'s `child_view_bubble`) — the transcript
        // itself starts empty and only accumulates the child's own turns.
        let mut state = AppState::new();
        let run_id = Uuid::new_v4();
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ChildAgentStarted {
                run_id,
                call_id: Uuid::new_v4(),
                agent_type: "reviewer".to_string(),
                task: "look into flaky test".to_string(),
                label: "Investigate flaky test".to_string(),
            }),
            &empty_textarea(),
        );
        assert!(state.child_conversations[&run_id].messages.is_empty());
    }

    #[test]
    fn child_tool_pending_auto_switches_view_and_arms_auto_return() {
        let mut state = AppState::new();
        state.mode = TuiMode::Streaming;
        let run_id = tracked_child(&mut state);
        let id = Uuid::new_v4();
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolPending {
                id,
                name: "write".to_string(),
                arguments: serde_json::json!({}),
                content_sections: vec![],
                title_arguments: vec![],
                run_id: Some(run_id),
            }),
            &empty_textarea(),
        );
        assert!(
            matches!(state.active_view, ConversationView::Child { run_id: r, .. } if r == run_id),
            "expected auto-switch to the child's view, got {:?}",
            state.active_view
        );
        assert!(state.auto_switched_to_child);

        // Resolving the pause (denial) auto-returns since the switch was
        // automatic, not a manual navigation.
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolDenied { id }),
            &empty_textarea(),
        );
        assert_eq!(state.active_view, ConversationView::Root);
        assert!(!state.auto_switched_to_child);
    }

    #[test]
    fn manual_navigation_to_child_view_prevents_auto_return() {
        // A user who already drilled in manually (e.g. clicked the spawn
        // bubble) should not get yanked back to Root the moment the child's
        // pending tool call resolves — only an *auto*-switch re-arms
        // auto-return.
        let mut state = AppState::new();
        state.mode = TuiMode::Streaming;
        let run_id = tracked_child(&mut state);
        state.switch_to_child_view(run_id);
        assert!(!state.auto_switched_to_child);

        let id = Uuid::new_v4();
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolPending {
                id,
                name: "write".to_string(),
                arguments: serde_json::json!({}),
                content_sections: vec![],
                title_arguments: vec![],
                run_id: Some(run_id),
            }),
            &empty_textarea(),
        );
        assert!(matches!(state.active_view, ConversationView::Child { .. }));
        assert!(!state.auto_switched_to_child);

        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolDenied { id }),
            &empty_textarea(),
        );
        assert!(
            matches!(state.active_view, ConversationView::Child { .. }),
            "manual navigation must not auto-return: {:?}",
            state.active_view
        );
    }

    #[test]
    fn root_tool_pending_while_drilled_into_a_child_auto_switches_to_root_and_restores_the_child_view_on_resolve(
    ) {
        // Regression: a root-owned approval must not be stranded behind a
        // subagent's transcript the user happens to be drilled into — it
        // has to pull the view back to Root, then restore the child view
        // once the approval resolves.
        let mut state = AppState::new();
        state.mode = TuiMode::Streaming;
        let run_id = tracked_child(&mut state);
        state.switch_to_child_view(run_id);
        assert!(!state.auto_switched_to_child);

        let id = Uuid::new_v4();
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolPending {
                id,
                name: "bash".to_string(),
                arguments: serde_json::json!({}),
                content_sections: vec![],
                title_arguments: vec![],
                run_id: None,
            }),
            &empty_textarea(),
        );
        assert_eq!(
            state.active_view,
            ConversationView::Root,
            "expected auto-switch to root for the root-owned approval"
        );
        assert!(state.auto_switched_to_child);

        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolDenied { id }),
            &empty_textarea(),
        );
        assert!(
            matches!(state.active_view, ConversationView::Child { run_id: r, .. } if r == run_id),
            "expected to return to the previously-viewed child, got {:?}",
            state.active_view
        );
        assert!(!state.auto_switched_to_child);
    }

    #[test]
    fn child_tool_pending_for_a_different_running_child_auto_switches_away_from_a_finished_childs_view_and_restores_it_on_resolve(
    ) {
        // Regression: with two subagents spawned, one already finished (and
        // the user manually drilled into its transcript) and a second still
        // running, an approval belonging to the *second* (background) child
        // must not be stranded behind the finished child's transcript — it
        // has to switch to the child that actually needs the approval, then
        // return to exactly the view the user was on before.
        let mut state = AppState::new();
        state.mode = TuiMode::Streaming;

        // First child finishes; user manually drills into its (now static)
        // transcript.
        let finished_run_id = tracked_child(&mut state);
        state.running_child = None;
        state.switch_to_child_view(finished_run_id);
        assert!(!state.auto_switched_to_child);

        // Second child starts running in the background and hits a bash
        // approval.
        let running_run_id = Uuid::new_v4();
        state.start_child_run(crate::tui_reducer_state::ChildRunInfo {
            run_id: running_run_id,
            bubble_id: Uuid::new_v4(),
            agent_type: "reviewer".to_string(),
            task: "run the test suite".to_string(),
            label: "Run tests".to_string(),
        });
        let id = Uuid::new_v4();
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolPending {
                id,
                name: "bash".to_string(),
                arguments: serde_json::json!({}),
                content_sections: vec![],
                title_arguments: vec![],
                run_id: Some(running_run_id),
            }),
            &empty_textarea(),
        );

        assert!(
            matches!(state.active_view, ConversationView::Child { run_id: r, .. } if r == running_run_id),
            "expected auto-switch to the child awaiting approval, got {:?}",
            state.active_view
        );
        assert!(state.auto_switched_to_child);

        // Resolving the approval must return to the finished child's view —
        // not Root, since that's what the user was actually looking at.
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolDenied { id }),
            &empty_textarea(),
        );
        assert!(
            matches!(state.active_view, ConversationView::Child { run_id: r, .. } if r == finished_run_id),
            "expected to return to the previously-viewed finished child, got {:?}",
            state.active_view
        );
        assert!(!state.auto_switched_to_child);
    }

    #[test]
    fn esc_with_empty_draft_in_child_view_navigates_back_to_root() {
        let mut state = AppState::new();
        state.mode = TuiMode::Streaming;
        let run_id = tracked_child(&mut state);
        state.switch_to_child_view(run_id);
        assert!(matches!(state.active_view, ConversationView::Child { .. }));

        let effects = update(
            &mut state,
            TuiEvent::Input(InputEvent::Escape),
            &empty_textarea(),
        );

        assert_eq!(state.active_view, ConversationView::Root);
        assert!(
            effects.is_empty(),
            "back-navigation must not touch TuiMode/Aborting effects: {effects:?}"
        );
        assert_eq!(
            state.mode,
            TuiMode::Streaming,
            "the root agent's own busy state must be left untouched by back-navigation"
        );
    }

    #[test]
    fn esc_with_non_empty_draft_in_child_view_clears_draft_instead_of_navigating() {
        let mut state = AppState::new();
        let run_id = tracked_child(&mut state);
        state.switch_to_child_view(run_id);

        let effects = update(
            &mut state,
            TuiEvent::Input(InputEvent::Escape),
            &textarea_with("some draft"),
        );

        assert!(matches!(state.active_view, ConversationView::Child { .. }));
        assert!(matches!(effects.as_slice(), [Effect::ClearTextarea]));
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
            armed_by: ConfirmKey::CtrlC,
        };
        let effects = update(
            &mut state,
            TuiEvent::Input(InputEvent::CtrlC),
            &empty_textarea(),
        );
        assert!(effects.contains(&Effect::Quit));
    }

    #[test]
    fn escape_after_ctrl_c_while_idle_clears_draft_instead_of_confirming_quit() {
        // Regression: pressing Ctrl-C once (arming the quit confirm), then
        // Esc, must not be treated as the confirming second press. Esc
        // dismisses the pending quit and clears the draft instead.
        let mut state = AppState::new();
        state.mode = TuiMode::Idle;
        update(
            &mut state,
            TuiEvent::Input(InputEvent::CtrlC),
            &empty_textarea(),
        );
        assert!(matches!(state.mode, TuiMode::DoublePressAbort { .. }));

        let effects = update(
            &mut state,
            TuiEvent::Input(InputEvent::Escape),
            &empty_textarea(),
        );
        assert!(!effects.contains(&Effect::Quit));
        assert_eq!(state.mode, TuiMode::Idle);
    }

    #[test]
    fn ctrl_c_after_escape_while_streaming_does_not_quit() {
        // Regression: pressing Esc once (arming the cancel-only confirm),
        // then Ctrl-C, must not be treated as Ctrl-C's confirming second
        // press. Ctrl-C instead starts its own fresh confirm window.
        let mut state = AppState::new();
        state.mode = TuiMode::Streaming;
        update(
            &mut state,
            TuiEvent::Input(InputEvent::Escape),
            &empty_textarea(),
        );
        assert!(matches!(
            state.mode,
            TuiMode::DoublePressAbort {
                armed_by: ConfirmKey::Escape,
                ..
            }
        ));

        let effects = update(
            &mut state,
            TuiEvent::Input(InputEvent::CtrlC),
            &empty_textarea(),
        );
        assert!(!effects.contains(&Effect::StopAgent));
        assert!(!state.aborting_exit);
        assert!(matches!(
            state.mode,
            TuiMode::DoublePressAbort {
                armed_by: ConfirmKey::CtrlC,
                ..
            }
        ));
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
            armed_by: ConfirmKey::CtrlC,
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
            TuiEvent::Domain(DomainEvent::ToolKilled {
                id: Uuid::new_v4(),
                run_id: None,
            }),
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
        assert!(effects.contains(&Effect::DenyTool {
            id: tool_id,
            reason: String::new(),
        }));
        assert!(effects.contains(&Effect::RestoreInputState));
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
        assert!(effects.contains(&Effect::DenyTool {
            id: tool_id,
            reason: String::new(),
        }));
        assert!(effects.contains(&Effect::RestoreInputState));
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
    fn enter_with_text_in_child_view_is_a_no_op() {
        // Regression test: Enter must not silently steer the *root* agent
        // while the user is looking at a subagent's drill-in transcript —
        // there is no visual confirmation of which conversation the message
        // would go to, and the child conversation itself does not accept
        // input.
        let mut state = AppState::new();
        state.mode = TuiMode::Streaming;
        let run_id = tracked_child(&mut state);
        state.switch_to_child_view(run_id);

        let effects = update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Enter))),
            &textarea_with("hello"),
        );

        assert!(effects.is_empty(), "expected no effects, got {effects:?}");
        assert!(
            state.queued_steer_texts.is_empty(),
            "must not queue a steer to the root agent"
        );
        assert_eq!(
            state.mode,
            TuiMode::Streaming,
            "must not touch the root agent's TuiMode"
        );
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
        assert!(effects.contains(&Effect::RestoreInputState));
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
        assert!(effects.contains(&Effect::DenyTool {
            id: tool_id,
            reason: "deny reason".to_string(),
        }));
        assert!(effects.contains(&Effect::RestoreInputState));
    }

    #[test]
    fn domain_clarify_pending_sets_tui_mode() {
        let mut state = AppState::new();
        state.clarify_banner_state.clarify_scroll_offset = 5; // stale value from a prior clarify banner
        let effects = update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ClarifyPending {
                id: Uuid::new_v4(),
                question: "Test?".to_string(),
                context: None,
                choices: None,
                run_id: None,
            }),
            &empty_textarea(),
        );
        assert!(effects.contains(&Effect::SaveInputStateAndClear));
        assert!(matches!(
            state.mode,
            TuiMode::ClarifyPending { ref question, choices: None, .. } if question == "Test?"
        ));
        // Scrolled to the bottom (clamped to max_scroll on render) so the
        // question and choices are visible immediately, not the stale offset.
        assert_eq!(state.clarify_banner_state.clarify_scroll_offset, usize::MAX);
    }

    #[test]
    fn domain_pending_clarify_pending_resets_scroll_to_bottom() {
        let mut state = AppState::new();
        state.mode = TuiMode::Pending;
        state.clarify_banner_state.clarify_scroll_offset = 5; // stale value from a prior clarify banner
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ClarifyPending {
                id: Uuid::new_v4(),
                question: "Test?".to_string(),
                context: None,
                choices: None,
                run_id: None,
            }),
            &empty_textarea(),
        );
        assert!(matches!(state.mode, TuiMode::ClarifyPending { .. }));
        assert_eq!(state.clarify_banner_state.clarify_scroll_offset, usize::MAX);
    }

    #[test]
    fn domain_streaming_clarify_pending_resets_scroll_to_bottom() {
        let mut state = AppState::new();
        state.mode = TuiMode::Streaming;
        state.clarify_banner_state.clarify_scroll_offset = 5; // stale value from a prior clarify banner
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ClarifyPending {
                id: Uuid::new_v4(),
                question: "Test?".to_string(),
                context: None,
                choices: None,
                run_id: None,
            }),
            &empty_textarea(),
        );
        assert!(matches!(state.mode, TuiMode::ClarifyPending { .. }));
        assert_eq!(state.clarify_banner_state.clarify_scroll_offset, usize::MAX);
    }

    #[test]
    fn clarify_digit_passes_through_to_textarea() {
        let mut state = AppState::new();
        state.mode = TuiMode::ClarifyPending {
            id: Uuid::new_v4(),
            question: "Pick one".to_string(),
            context: None,
            choices: Some(vec!["alpha".to_string(), "beta".to_string()]),
        };
        let key = key_event(KeyCode::Char('2'));
        let effects = update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key)),
            &empty_textarea(),
        );
        assert_eq!(
            effects,
            vec![Effect::TextareaKey(key_event(KeyCode::Char('2')))]
        );
    }

    #[test]
    fn clarify_enter_answers_and_restores_textarea() {
        let mut state = AppState::new();
        state.mode = TuiMode::ClarifyPending {
            id: Uuid::new_v4(),
            question: "Pick one".to_string(),
            context: None,
            choices: None,
        };
        let effects = update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Enter))),
            &textarea_with("free-form answer"),
        );
        assert_eq!(state.mode, TuiMode::Pending);
        assert_eq!(
            effects,
            vec![
                Effect::AnswerClarify("free-form answer".to_string()),
                Effect::RestoreInputState,
            ]
        );
    }

    #[test]
    fn clarify_esc_with_empty_input_aborts_question() {
        let mut state = AppState::new();
        state.mode = TuiMode::ClarifyPending {
            id: Uuid::new_v4(),
            question: "Pick one".to_string(),
            context: None,
            choices: None,
        };
        let effects = update(
            &mut state,
            TuiEvent::Input(InputEvent::Escape),
            &empty_textarea(),
        );
        assert_eq!(state.mode, TuiMode::Pending);
        assert!(effects.contains(&Effect::AbortClarify));
        assert!(effects.contains(&Effect::RestoreInputState));
    }

    #[test]
    fn clarify_esc_with_non_empty_input_clears_input_only() {
        let mut state = AppState::new();
        state.mode = TuiMode::ClarifyPending {
            id: Uuid::new_v4(),
            question: "Pick one".to_string(),
            context: None,
            choices: None,
        };
        let effects = update(
            &mut state,
            TuiEvent::Input(InputEvent::Escape),
            &textarea_with("partial answer"),
        );
        assert!(
            matches!(state.mode, TuiMode::ClarifyPending { .. }),
            "mode must stay ClarifyPending when input is not empty"
        );
        assert_eq!(effects, vec![Effect::ClearTextarea]);
    }

    #[test]
    fn clarify_choice_click_fills_textarea() {
        let mut state = AppState::new();
        state.mode = TuiMode::ClarifyPending {
            id: Uuid::new_v4(),
            question: "Pick?".to_string(),
            context: None,
            choices: Some(vec!["alpha".to_string(), "beta".to_string()]),
        };
        // Banner starts at row 5, width 40.
        // Row 5: "Pick?" (question), row 6: "1. alpha" (choices start at content row 1).
        state.layout.clarify_banner = ratatui::layout::Rect::new(0, 5, 40, 4);

        let mouse_down = crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 5,
            row: 6,
            modifiers: KeyModifiers::NONE,
        };
        let effects_down = update(&mut state, TuiEvent::Mouse(mouse_down), &empty_textarea());
        assert!(effects_down.is_empty(), "mousedown alone must not select");
        assert_eq!(
            state.clarify_banner_state.clarify_choice_mousedown,
            Some((5, 6))
        );

        let mouse_up = crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
            column: 5,
            row: 6,
            modifiers: KeyModifiers::NONE,
        };
        let effects_up = update(&mut state, TuiEvent::Mouse(mouse_up), &empty_textarea());
        assert_eq!(
            effects_up,
            vec![Effect::SetInputText("alpha".to_string())],
            "mouseup at same position must fill textarea with choice text"
        );
        assert!(state
            .clarify_banner_state
            .clarify_choice_mousedown
            .is_none());
    }

    #[test]
    fn clarify_choice_click_on_blank_area_does_not_select() {
        let mut state = AppState::new();
        state.mode = TuiMode::ClarifyPending {
            id: Uuid::new_v4(),
            question: "Pick?".to_string(),
            context: None,
            choices: Some(vec!["alpha".to_string()]),
        };
        // Banner at x=0, y=5, width=40.
        // "1. alpha" is 8 chars wide; clicking at col 20 is past the text.
        state.layout.clarify_banner = ratatui::layout::Rect::new(0, 5, 40, 3);

        let mouse_down = crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 20,
            row: 6,
            modifiers: KeyModifiers::NONE,
        };
        update(&mut state, TuiEvent::Mouse(mouse_down), &empty_textarea());

        let mouse_up = crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
            column: 20,
            row: 6,
            modifiers: KeyModifiers::NONE,
        };
        let effects_up = update(&mut state, TuiEvent::Mouse(mouse_up), &empty_textarea());
        assert!(
            effects_up.is_empty(),
            "click past choice text must not select the choice; got {effects_up:?}"
        );
    }

    #[test]
    fn clarify_choice_click_with_context_offsets_correctly() {
        let mut state = AppState::new();
        // Context is 1 row, spacer is 1 row, question is 1 row → choices start at banner.y + 3.
        state.mode = TuiMode::ClarifyPending {
            id: Uuid::new_v4(),
            question: "Pick an animal?".to_string(),
            context: Some("Context line.".to_string()),
            choices: Some(vec!["rhino".to_string(), "panda".to_string()]),
        };
        // banner.y = 5, width = 40 → context row 5, context→question spacer row 6,
        // question row 7, choices at 8 and 9.
        state.layout.clarify_banner = ratatui::layout::Rect::new(0, 5, 40, 7);

        let click_row = 8u16; // first choice row
        let mouse_down = crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 3,
            row: click_row,
            modifiers: KeyModifiers::NONE,
        };
        update(&mut state, TuiEvent::Mouse(mouse_down), &empty_textarea());

        let mouse_up = crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
            column: 3,
            row: click_row,
            modifiers: KeyModifiers::NONE,
        };
        let effects_up = update(&mut state, TuiEvent::Mouse(mouse_up), &empty_textarea());
        assert_eq!(
            effects_up,
            vec![Effect::SetInputText("rhino".to_string())],
            "click on first choice row with context present must select first choice"
        );
    }

    #[test]
    fn clarify_choice_click_with_scroll_offset_selects_correct_choice() {
        let mut state = AppState::new();
        // context "line one\nline two" → 1 rendered line (soft break joins)
        // context→question spacer = 1 row
        // question "Pick?" → 1 rendered line
        // choices start at content row 3.
        // With scroll_offset=3 the banner top shows the first choice at visual row banner.y.
        state.mode = TuiMode::ClarifyPending {
            id: Uuid::new_v4(),
            question: "Pick?".to_string(),
            context: Some("line one\nline two".to_string()),
            choices: Some(vec!["alpha".to_string(), "beta".to_string()]),
        };
        state.layout.clarify_banner = ratatui::layout::Rect::new(0, 5, 40, 2);
        state.clarify_banner_state.clarify_scroll_offset = 3; // scrolled so first choice is at visual top

        let click_row = 5u16; // banner.y — visually the first choice
        let mouse_down = crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 3,
            row: click_row,
            modifiers: KeyModifiers::NONE,
        };
        update(&mut state, TuiEvent::Mouse(mouse_down), &empty_textarea());

        let mouse_up = crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
            column: 3,
            row: click_row,
            modifiers: KeyModifiers::NONE,
        };
        let effects_up = update(&mut state, TuiEvent::Mouse(mouse_up), &empty_textarea());
        assert_eq!(
            effects_up,
            vec![Effect::SetInputText("alpha".to_string())],
            "click at visual top of scrolled banner must resolve to first choice"
        );
    }

    #[test]
    fn clarify_choice_drag_copies_text_to_clipboard() {
        let mut state = AppState::new();
        state.mode = TuiMode::ClarifyPending {
            id: Uuid::new_v4(),
            question: "Pick?".to_string(),
            context: None,
            choices: Some(vec!["alpha".to_string(), "beta".to_string()]),
        };
        // Row 5: "Pick?", row 6: "1. alpha", row 7: "2. beta".
        state.layout.clarify_banner = ratatui::layout::Rect::new(0, 5, 40, 4);

        let mouse_down = crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 5,
            row: 6,
            modifiers: KeyModifiers::NONE,
        };
        update(&mut state, TuiEvent::Mouse(mouse_down), &empty_textarea());

        // Drag to a different column — simulates text selection.
        let mouse_drag = crossterm::event::MouseEvent {
            kind: MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            column: 15,
            row: 6,
            modifiers: KeyModifiers::NONE,
        };
        update(&mut state, TuiEvent::Mouse(mouse_drag), &empty_textarea());
        assert_eq!(
            state.clarify_banner_state.clarify_banner_drag_end,
            Some((15, 6))
        );

        let mouse_up = crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
            column: 15,
            row: 6,
            modifiers: KeyModifiers::NONE,
        };
        let effects_up = update(&mut state, TuiEvent::Mouse(mouse_up), &empty_textarea());
        assert!(
            effects_up.contains(&Effect::CopyToClipboard("pha".to_string())),
            "drag must copy selected banner text (trimmed to dragged column range) to clipboard"
        );
        assert!(state
            .clarify_banner_state
            .clarify_choice_mousedown
            .is_none());
        assert!(state.clarify_banner_state.clarify_banner_drag_end.is_none());
    }

    #[test]
    fn clarify_choice_multiline_drag_copies_trimmed_text() {
        let mut state = AppState::new();
        state.mode = TuiMode::ClarifyPending {
            id: Uuid::new_v4(),
            question: "Pick?".to_string(),
            context: None,
            choices: Some(vec!["alpha".to_string(), "beta".to_string()]),
        };
        // Banner at x=0, y=5, width=40.
        // Row 5: "Pick?" (question), row 6: "1. alpha", row 7: "2. beta"
        state.layout.clarify_banner = ratatui::layout::Rect::new(0, 5, 40, 4);

        // Drag starts at col=4, row=6 ("1. alpha"[4..] = "lpha") and ends
        // at col=7, row=7 ("2. beta"[0..7] = "2. beta").
        let mouse_down = crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 4,
            row: 6,
            modifiers: KeyModifiers::NONE,
        };
        update(&mut state, TuiEvent::Mouse(mouse_down), &empty_textarea());

        let mouse_drag = crossterm::event::MouseEvent {
            kind: MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            column: 7,
            row: 7,
            modifiers: KeyModifiers::NONE,
        };
        update(&mut state, TuiEvent::Mouse(mouse_drag), &empty_textarea());

        let mouse_up = crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
            column: 7,
            row: 7,
            modifiers: KeyModifiers::NONE,
        };
        let effects_up = update(&mut state, TuiEvent::Mouse(mouse_up), &empty_textarea());
        assert!(
            effects_up.contains(&Effect::CopyToClipboard("lpha\n2. beta".to_string())),
            "multiline drag must copy first line trimmed from start col, last line trimmed to end col; got {effects_up:?}"
        );
    }

    // ── Help scroll ───────────────────────────────────────────────────────────

    #[test]
    fn keyboard_in_help_mode_scrolls_help() {
        let mut state = AppState::new();
        state.focus = Focus::HelpPopup;
        state.help_popup.help_scroll_offset = 5;
        update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Down))),
            &empty_textarea(),
        );
        assert_eq!(state.help_popup.help_scroll_offset, 6);
    }

    // ── Mouse scroll ──────────────────────────────────────────────────────────

    #[test]
    fn mouse_click_on_scrollbar_respects_help_and_slash_overlay_priority() {
        for help_popup in [false, true] {
            let mut state = AppState::new();
            state.conversation.scroll_offset = 50;
            state.conversation.scroll_locked = false;
            state.layout.chat_scrollbar = crate::tui_reducer_state::ChatScrollbarLayout {
                rect: ratatui::layout::Rect::new(79, 3, 1, 7),
                maximum_scroll_offset: 100,
            };
            if help_popup {
                state.focus = Focus::HelpPopup;
                state.layout.help_popup.rect = ratatui::layout::Rect::new(10, 2, 30, 8);
            } else {
                state.slash_overlay = SlashOverlay::Suggestions {
                    choices: Vec::new(),
                    selected: 0,
                };
                state.layout.slash_overlay.content_area = ratatui::layout::Rect::new(70, 2, 10, 8);
            }
            let effects = update(
                &mut state,
                TuiEvent::Mouse(crossterm::event::MouseEvent {
                    kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
                    column: 79,
                    row: 3,
                    modifiers: KeyModifiers::NONE,
                }),
                &empty_textarea(),
            );
            assert!(effects.is_empty());
            assert_eq!(state.conversation.scroll_offset, 50);
            assert!(!state.conversation.scroll_locked);
            assert!(!state.conversation.chat_drag_active);
            if help_popup {
                assert_eq!(state.focus, Focus::MainInput);
            } else {
                assert_eq!(state.slash_overlay_drag_start, Some((79, 3)));
            }
        }
    }

    #[test]
    fn mouse_scroll_up_in_chat_area_decrements_scroll_offset() {
        let mut state = AppState::new();
        state.conversation.scroll_offset = 5;
        state.conversation.scroll_locked = false;
        let mouse = crossterm::event::MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 10,
            row: 10,
            modifiers: KeyModifiers::NONE,
        };
        update(&mut state, TuiEvent::Mouse(mouse), &empty_textarea());
        assert_eq!(state.conversation.scroll_offset, 4);
    }

    #[test]
    fn mouse_scroll_over_model_picker_moves_picker_selection_instead_of_chat() {
        let mut state = AppState::new();
        state.slash_overlay = SlashOverlay::ModelPicker {
            choices: (0..3)
                .map(|index| ModelChoice {
                    label: format!("stub/model-{index}"),
                    initial_reasoning_effort: None,
                    selection: ProviderModelOption {
                        provider_connection: "stub".to_string(),
                        model: crate::llm::ModelInfo {
                            id: format!("model-{index}"),
                            context_length: None,
                            reasoning_efforts: None,
                            reasoning_default: None,
                            input_modalities: None,
                        },
                        reasoning_effort: None,
                    },
                    is_active: false,
                    engine_name: None,
                    selection_target: None,
                    selection_preview_error: None,
                })
                .collect(),
            filter: String::new(),
            selected: 0,
        };
        state.conversation.scroll_offset = 5;
        state.conversation.scroll_locked = false;
        let mouse = crossterm::event::MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 10,
            row: 10,
            modifiers: KeyModifiers::NONE,
        };

        update(&mut state, TuiEvent::Mouse(mouse), &empty_textarea());

        assert_eq!(
            state
                .selected_model()
                .map(|choice| choice.selection.model.id.as_str()),
            Some("model-1")
        );
        assert_eq!(
            state.conversation.scroll_offset, 5,
            "scrolling the model picker must not move the chat"
        );
    }

    #[test]
    fn mouse_click_on_model_picker_entry_selects_without_switching() {
        let model_choice = |model_id: &str| ModelChoice {
            label: format!("stub/{model_id}"),
            initial_reasoning_effort: None,
            selection: ProviderModelOption {
                provider_connection: "stub".to_string(),
                model: crate::llm::ModelInfo {
                    id: model_id.to_string(),
                    context_length: None,
                    reasoning_efforts: None,
                    reasoning_default: None,
                    input_modalities: None,
                },
                reasoning_effort: None,
            },
            is_active: false,
            engine_name: None,
            selection_target: None,
            selection_preview_error: None,
        };
        let mut state = AppState::new();
        state.slash_overlay = SlashOverlay::ModelPicker {
            choices: vec![model_choice("model-0"), model_choice("model-1")],
            filter: String::new(),
            selected: 0,
        };
        // With the input at row 10, the two-row picker renders its entries at
        // rows 7 and 8.
        state.layout.input_area.rect = ratatui::layout::Rect::new(0, 10, 40, 2);
        state.layout.slash_overlay.content_area = ratatui::layout::Rect::new(1, 7, 37, 2);
        let mouse = crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 5,
            row: 8,
            modifiers: KeyModifiers::NONE,
        };

        let effects = update(&mut state, TuiEvent::Mouse(mouse), &empty_textarea());

        assert_eq!(
            state
                .selected_model()
                .map(|choice| choice.selection.model.id.as_str()),
            Some("model-1")
        );
        assert!(
            effects.is_empty(),
            "clicking a model row must select it without confirming the switch: {effects:?}"
        );
    }

    #[test]
    fn slash_overlay_model_drag_copies_the_filtered_model_label() {
        let model_choice = |label: &str, provider_connection: &str, model_id: &str| ModelChoice {
            label: label.to_string(),
            initial_reasoning_effort: None,
            selection: ProviderModelOption {
                provider_connection: provider_connection.to_string(),
                model: crate::llm::ModelInfo {
                    id: model_id.to_string(),
                    context_length: None,
                    reasoning_efforts: None,
                    reasoning_default: None,
                    input_modalities: None,
                },
                reasoning_effort: None,
            },
            is_active: false,
            engine_name: None,
            selection_target: None,
            selection_preview_error: None,
        };
        let expected_text = "provider/provider-model";
        let mut state = AppState::new();
        state.slash_overlay = SlashOverlay::ModelPicker {
            choices: vec![
                model_choice("engine/engine-model", "engine", "engine-model"),
                model_choice(expected_text, "provider", "provider-model"),
            ],
            filter: "provider".to_string(),
            selected: 0,
        };
        state.layout.slash_overlay.content_area = ratatui::layout::Rect::new(1, 7, 37, 1);

        update(
            &mut state,
            TuiEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: 3,
                row: 7,
                modifiers: KeyModifiers::NONE,
            }),
            &empty_textarea(),
        );
        update(
            &mut state,
            TuiEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Drag(crossterm::event::MouseButton::Left),
                column: 3 + expected_text.chars().count() as u16,
                row: 7,
                modifiers: KeyModifiers::NONE,
            }),
            &empty_textarea(),
        );
        let effects = update(
            &mut state,
            TuiEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
                column: 3 + expected_text.chars().count() as u16,
                row: 7,
                modifiers: KeyModifiers::NONE,
            }),
            &empty_textarea(),
        );

        assert!(
            effects.iter().any(|effect| matches!(
                effect,
                Effect::CopyToClipboard(text) if text == expected_text
            )),
            "dragging a filtered model row must copy its rendered label: {effects:?}"
        );
    }

    #[test]
    fn slash_overlay_drag_emits_copy_to_clipboard_effect() {
        let mut state = AppState::new();
        state.slash_overlay = SlashOverlay::Suggestions {
            choices: vec![crate::slash_command::SLASH_COMMANDS[1]],
            selected: 0,
        };
        state.layout.slash_overlay.content_area = ratatui::layout::Rect::new(1, 7, 37, 1);

        let mouse_down = crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 3,
            row: 7,
            modifiers: KeyModifiers::NONE,
        };
        update(&mut state, TuiEvent::Mouse(mouse_down), &empty_textarea());

        let mouse_drag = crossterm::event::MouseEvent {
            kind: MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            column: 9,
            row: 7,
            modifiers: KeyModifiers::NONE,
        };
        update(&mut state, TuiEvent::Mouse(mouse_drag), &empty_textarea());

        let mouse_up = crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
            column: 9,
            row: 7,
            modifiers: KeyModifiers::NONE,
        };
        let effects = update(&mut state, TuiEvent::Mouse(mouse_up), &empty_textarea());

        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::CopyToClipboard(text) if text == "/model")),
            "dragging slash-command text must copy the selected range: {effects:?}"
        );
    }

    #[test]
    fn slash_overlay_drag_uses_the_scrolled_content_row() {
        let mut state = AppState::new();
        state.slash_overlay = SlashOverlay::Suggestions {
            choices: crate::slash_command::SLASH_COMMANDS.to_vec(),
            selected: 0,
        };
        state.layout.slash_overlay = crate::tui_reducer_state::SlashOverlayLayout {
            content_area: ratatui::layout::Rect::new(1, 5, 37, 2),
            first_visible_row: 1,
        };

        update(
            &mut state,
            TuiEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: 3,
                row: 5,
                modifiers: KeyModifiers::NONE,
            }),
            &empty_textarea(),
        );
        update(
            &mut state,
            TuiEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Drag(crossterm::event::MouseButton::Left),
                column: 9,
                row: 6,
                modifiers: KeyModifiers::NONE,
            }),
            &empty_textarea(),
        );
        let effects = update(
            &mut state,
            TuiEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
                column: 9,
                row: 6,
                modifiers: KeyModifiers::NONE,
            }),
            &empty_textarea(),
        );

        let expected_first_line = format!(
            "{:<8} {}",
            crate::slash_command::SLASH_COMMANDS[1].name,
            crate::slash_command::SLASH_COMMANDS[1].description
        );
        assert!(
            effects.iter().any(|effect| matches!(
                effect,
                Effect::CopyToClipboard(text)
                    if text == &format!("{expected_first_line}\n/tools")
            )),
            "a scrolled drag must map to the visible overlay rows: {effects:?}"
        );
    }

    #[test]
    fn mouse_scroll_over_header_bubble_scrolls_the_header_not_the_chat_area() {
        let mut state = AppState::new();
        state.layout.header_bubble = ratatui::layout::Rect::new(0, 0, 40, 4);
        state.child_view.header_bubble_scroll_offset = 2;
        state.conversation.scroll_offset = 5;
        state.conversation.scroll_locked = false;
        let mouse = crossterm::event::MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 10,
            row: 2,
            modifiers: KeyModifiers::NONE,
        };
        update(&mut state, TuiEvent::Mouse(mouse), &empty_textarea());
        assert_eq!(state.child_view.header_bubble_scroll_offset, 3);
        assert_eq!(
            state.conversation.scroll_offset, 5,
            "scroll over the header must not also move the chat area"
        );
    }

    #[test]
    fn click_on_header_bubble_toggles_its_collapsed_state() {
        use crate::tui_reducer_state::ToolCallState;

        let mut state = AppState::new();
        let bubble_id = Uuid::new_v4();
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
        let run_id = Uuid::new_v4();
        state.start_child_run(ChildRunInfo {
            run_id,
            bubble_id,
            agent_type: "reviewer".to_string(),
            task: "look into flaky test".to_string(),
            label: "Investigate flaky test".to_string(),
        });
        state.switch_to_child_view(run_id);
        state.layout.header_bubble = ratatui::layout::Rect::new(0, 0, 40, 4);
        let mouse_down = crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 5,
            row: 1,
            modifiers: KeyModifiers::NONE,
        };

        update(&mut state, TuiEvent::Mouse(mouse_down), &empty_textarea());
        update(
            &mut state,
            TuiEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
                column: 5,
                row: 1,
                modifiers: KeyModifiers::NONE,
            }),
            &empty_textarea(),
        );

        assert!(
            matches!(&state.chat_entries[0], ChatEntry::ToolBubble { collapsed, .. } if *collapsed),
            "click on the header bubble must toggle the real root bubble's collapsed state"
        );
    }

    #[test]
    fn drag_on_header_bubble_copies_text_without_toggling_collapse() {
        use crate::tui_reducer_state::ToolCallState;

        let mut state = AppState::new();
        let bubble_id = Uuid::new_v4();
        state.chat_entries.push(ChatEntry::ToolBubble {
            id: bubble_id,
            name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
            state: Box::new(ToolCallState::Running {
                arguments: serde_json::json!({
                    "agent_type": "reviewer",
                    "task": "inspect this task"
                }),
                content_sections: vec![],
                title_arguments: vec![
                    ("agent_type".to_string(), serde_json::json!("reviewer")),
                    ("task".to_string(), serde_json::json!("inspect this task")),
                ],
                progress_arguments: vec![],
            }),
            collapsed: false,
        });
        let run_id = Uuid::new_v4();
        state.start_child_run(ChildRunInfo {
            run_id,
            bubble_id,
            agent_type: "reviewer".to_string(),
            task: "inspect this task".to_string(),
            label: "Inspect task".to_string(),
        });
        state.switch_to_child_view(run_id);
        render_app_state_buffer(&mut state);
        let task_line = state
            .child_view
            .header_rendered_lines
            .iter()
            .position(|line| line.contains("inspect this task"))
            .expect("rendered header must contain the task");
        let task_column = state.child_view.header_rendered_line_prefix_widths[task_line];
        let task_text_column = state.child_view.header_rendered_lines[task_line]
            .find("inspect")
            .expect("rendered header must contain the selected word");
        let task_row = state.layout.header_bubble.y + task_line as u16;
        let selection_start =
            state.layout.header_bubble.x + (task_column + task_text_column) as u16;
        let selection_end = selection_start + "inspect".chars().count() as u16;

        update(
            &mut state,
            TuiEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: selection_start,
                row: task_row,
                modifiers: KeyModifiers::NONE,
            }),
            &empty_textarea(),
        );
        assert!(
            matches!(&state.chat_entries[0], ChatEntry::ToolBubble { collapsed, .. } if !*collapsed),
            "header mouse-down must not toggle collapse before the drag is resolved"
        );

        update(
            &mut state,
            TuiEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Drag(crossterm::event::MouseButton::Left),
                column: selection_end,
                row: task_row,
                modifiers: KeyModifiers::NONE,
            }),
            &empty_textarea(),
        );
        let rendered = render_app_state_buffer(&mut state);
        assert_eq!(
            rendered
                .cell((selection_start, task_row))
                .expect("selected header cell")
                .style()
                .bg,
            Some(crate::theme::CHAT_SELECTION_BG),
            "the selected header range must be highlighted"
        );
        let effects = update(
            &mut state,
            TuiEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
                column: selection_end,
                row: task_row,
                modifiers: KeyModifiers::NONE,
            }),
            &empty_textarea(),
        );

        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::CopyToClipboard(text) if text == "inspect")),
            "header drag must copy its selected text: {effects:?}"
        );
        assert!(
            matches!(&state.chat_entries[0], ChatEntry::ToolBubble { collapsed, .. } if !*collapsed),
            "a non-empty header drag must not toggle collapse"
        );
        assert!(!state.child_view.header_drag_active);
    }

    #[test]
    fn click_outside_header_bubble_does_not_toggle_it() {
        use crate::tui_reducer_state::ToolCallState;

        let mut state = AppState::new();
        let bubble_id = Uuid::new_v4();
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
        let run_id = Uuid::new_v4();
        state.start_child_run(ChildRunInfo {
            run_id,
            bubble_id,
            agent_type: "reviewer".to_string(),
            task: "look into flaky test".to_string(),
            label: "Investigate flaky test".to_string(),
        });
        state.switch_to_child_view(run_id);
        state.layout.header_bubble = ratatui::layout::Rect::new(0, 0, 40, 4);
        let mouse_down = crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 5,
            row: 10,
            modifiers: KeyModifiers::NONE,
        };

        update(&mut state, TuiEvent::Mouse(mouse_down), &empty_textarea());

        assert!(
            matches!(&state.chat_entries[0], ChatEntry::ToolBubble { collapsed, .. } if !*collapsed),
            "click below the header bubble must not toggle it"
        );
    }

    #[test]
    fn child_engine_configuration_is_retained_for_its_conversation_view() {
        let mut state = AppState::new();
        let run_id = Uuid::new_v4();

        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ChildAgentEngineConfigured {
                run_id,
                engine: crate::llm::engine::EngineDisplayInfo {
                    engine_name: "ollama/local2".to_string(),
                    provider_connection: "ollama".to_string(),
                    model: "child-model".to_string(),
                    reasoning_effort: None,
                    compaction_engine: None,
                    inherits_parent_runtime: false,
                },
            }),
            &empty_textarea(),
        );

        assert_eq!(
            state.child_engine_displays.get(&run_id),
            Some(&crate::llm::engine::EngineDisplayInfo {
                engine_name: "ollama/local2".to_string(),
                provider_connection: "ollama".to_string(),
                model: "child-model".to_string(),
                reasoning_effort: None,
                compaction_engine: None,
                inherits_parent_runtime: false,
            })
        );
    }

    #[test]
    fn header_bubble_stays_expanded_after_the_subagent_finishes() {
        // Regression: a bubble the user manually expanded while the subagent
        // was still running must stay expanded once it finishes — the user
        // may still want to inspect the header (task/result) after the fact.
        let bubble_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        let mut state = AppState::new();

        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ChildAgentStarted {
                run_id,
                call_id: bubble_id,
                agent_type: "reviewer".to_string(),
                task: "look into flaky test".to_string(),
                label: "Investigate flaky test".to_string(),
            }),
            &empty_textarea(),
        );
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolStart {
                id: bubble_id,
                name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
                arguments: serde_json::json!({}),
                content_sections: vec![],
                title_arguments: vec![],
                run_id: None,
            }),
            &empty_textarea(),
        );
        assert!(
            matches!(&state.chat_entries[0], ChatEntry::ToolBubble { collapsed, .. } if *collapsed),
            "spawn_subagent bubbles start collapsed by default"
        );

        // User manually expands the header bubble while the subagent runs.
        state.switch_to_child_view(run_id);
        state.layout.header_bubble = ratatui::layout::Rect::new(0, 0, 40, 4);
        update(
            &mut state,
            TuiEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: 5,
                row: 1,
                modifiers: KeyModifiers::NONE,
            }),
            &empty_textarea(),
        );
        update(
            &mut state,
            TuiEvent::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
                column: 5,
                row: 1,
                modifiers: KeyModifiers::NONE,
            }),
            &empty_textarea(),
        );
        assert!(
            matches!(&state.chat_entries[0], ChatEntry::ToolBubble { collapsed, .. } if !*collapsed),
            "click must have expanded the header bubble"
        );

        // The subagent finishes: its final `ToolDone` (the spawn_subagent
        // call itself completing) and `ChildAgentDone` (bookkeeping) arrive.
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolDone {
                id: bubble_id,
                name: crate::agent::tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
                result_section: ResultSection::plain(String::new()),
                summary: vec![],
                duration: std::time::Duration::from_secs(1),
                content_sections: vec![],
                run_id: None,
            }),
            &empty_textarea(),
        );
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ChildAgentDone {
                run_id,
                status: "done".to_string(),
                result: None,
            }),
            &empty_textarea(),
        );

        assert!(
            matches!(&state.chat_entries[0], ChatEntry::ToolBubble { collapsed, .. } if !*collapsed),
            "the header bubble must still be expanded after the subagent finishes"
        );
    }

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
        state.conversation.chat_drag_active = true;
        state.conversation.chat_rendered_lines = vec!["hello world".to_string()];
        state.conversation.chat_selection = Some(ChatSelection {
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
        assert!(
            !state.conversation.chat_drag_active,
            "drag should be cleared"
        );
    }

    #[test]
    fn help_popup_drag_mouse_up_emits_copy_to_clipboard_effect() {
        let mut state = AppState::new();
        state.focus = Focus::HelpPopup;
        state.layout.help_popup.rect = ratatui::layout::Rect::new(8, 3, 24, 6);
        state.layout.help_popup.content_area = ratatui::layout::Rect::new(10, 5, 20, 3);
        state.help_popup.help_rendered_lines = vec!["hello world".to_string()];

        let mouse_down = crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 10,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        update(&mut state, TuiEvent::Mouse(mouse_down), &empty_textarea());
        let mouse_drag = crossterm::event::MouseEvent {
            kind: MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            column: 15,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        update(&mut state, TuiEvent::Mouse(mouse_drag), &empty_textarea());
        let mouse_up = crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
            column: 15,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        let effects = update(&mut state, TuiEvent::Mouse(mouse_up), &empty_textarea());

        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::CopyToClipboard(text) if text == "hello")),
            "expected CopyToClipboard(\"hello\"), got {effects:?}"
        );
        assert!(
            !state.help_popup.help_drag_active,
            "help popup drag should be cleared"
        );
    }

    #[test]
    fn click_on_thinking_bubble_title_row_toggles_collapsed() {
        let mut state = AppState::new();
        state.chat_entries.push(ChatEntry::ThinkingMessage {
            text: "reasoning".to_string(),
            collapsed: true,
            duration: std::time::Duration::from_secs(1),
        });
        // (start_row=0, end_row=2, entry_index=0) — title row is start (no
        // top-padding row separates the bubble start from its title).
        state.conversation.thinking_title_rows = vec![(0, 2, 0)];
        state.conversation.chat_drag_active = true;
        state.conversation.chat_rendered_lines = vec![String::new(), String::new(), String::new()];
        // A zero-width selection at the title row is a plain click, not a drag.
        state.conversation.chat_selection = Some(ChatSelection {
            anchor_line: 0,
            anchor_col: 3,
            cursor_line: 0,
            cursor_col: 3,
        });
        let mouse = crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
            column: 3,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        update(&mut state, TuiEvent::Mouse(mouse), &empty_textarea());

        assert!(matches!(
            &state.chat_entries[0],
            ChatEntry::ThinkingMessage { collapsed, .. } if !*collapsed
        ));
    }

    #[test]
    fn click_on_streaming_thinking_bubble_title_row_toggles_partial_collapsed() {
        let mut state = AppState::new();
        state.turn_buffer.partial_thinking_text = "pondering...".to_string();
        state.turn_buffer.partial_thinking_collapsed = false;
        // (start_row=0, end_row=2, sentinel index) — title row is start (no
        // top-padding row separates the bubble start from its title).
        state.conversation.thinking_title_rows = vec![(
            0,
            2,
            crate::tui_reducer_state::STREAMING_THINKING_ENTRY_INDEX,
        )];
        state.conversation.chat_drag_active = true;
        state.conversation.chat_rendered_lines = vec![String::new(), String::new(), String::new()];
        // A zero-width selection at the title row is a plain click, not a drag.
        state.conversation.chat_selection = Some(ChatSelection {
            anchor_line: 0,
            anchor_col: 3,
            cursor_line: 0,
            cursor_col: 3,
        });
        let mouse = crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
            column: 3,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        update(&mut state, TuiEvent::Mouse(mouse), &empty_textarea());

        assert!(state.turn_buffer.partial_thinking_collapsed);
    }

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

    #[test]
    fn single_click_on_spawn_bubble_title_toggles_collapsed_not_drill_in() {
        // A single click must behave exactly like every other bubble
        // (collapse/expand) — drilling in requires a second, fast click. The
        // toggle itself is deferred until the double-click window expires
        // (see `BubbleClickWindowExpired`) so bubble layout stays stable
        // while a confirming second click could still arrive.
        let mut state = AppState::new();
        let bubble_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        state.start_child_run(ChildRunInfo {
            run_id,
            bubble_id,
            agent_type: "reviewer".to_string(),
            task: "look into flaky test".to_string(),
            label: "Investigate flaky test".to_string(),
        });
        let mouse = spawn_bubble_click_fixture(&mut state, bubble_id);

        update(&mut state, TuiEvent::Mouse(mouse), &empty_textarea());

        assert_eq!(state.active_view, ConversationView::Root);
        assert!(matches!(
            &state.chat_entries[0],
            ChatEntry::ToolBubble { collapsed, .. } if !*collapsed
        ));
        assert!(state.last_bubble_click.is_some());

        // The window expires with no confirming second click — now the
        // deferred toggle commits.
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::BubbleClickWindowExpired),
            &empty_textarea(),
        );

        assert!(matches!(
            &state.chat_entries[0],
            ChatEntry::ToolBubble { collapsed, .. } if *collapsed
        ));
        assert!(state.last_bubble_click.is_none());
    }

    #[test]
    fn double_click_on_spawn_bubble_title_drills_into_child_view() {
        let mut state = AppState::new();
        let bubble_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        state.start_child_run(ChildRunInfo {
            run_id,
            bubble_id,
            agent_type: "reviewer".to_string(),
            task: "look into flaky test".to_string(),
            label: "Investigate flaky test".to_string(),
        });
        let mouse = spawn_bubble_click_fixture(&mut state, bubble_id);

        // First click: arms double-click detection, defers the toggle.
        update(&mut state, TuiEvent::Mouse(mouse), &empty_textarea());
        // A zero-width selection at the title row is a plain click, not a drag.
        state.conversation.chat_drag_active = true;
        state.conversation.chat_selection = Some(ChatSelection {
            anchor_line: 0,
            anchor_col: 3,
            cursor_line: 0,
            cursor_col: 3,
        });
        // Second click, same bubble, well within the double-click window.
        update(&mut state, TuiEvent::Mouse(mouse), &empty_textarea());

        assert!(matches!(
            state.active_view,
            ConversationView::Child { run_id: r, .. } if r == run_id
        ));
        // The deferred toggle from the first click must not have committed —
        // the bubble was never collapsed, since the pair resolved as a
        // drill-in instead.
        assert!(matches!(
            &state.chat_entries[0],
            ChatEntry::ToolBubble { collapsed, .. } if !*collapsed
        ));
    }

    #[test]
    fn double_click_on_finished_spawn_bubble_still_drills_in_via_child_conversations() {
        // Once the child finishes, `running_child` is cleared but its entry
        // in `child_conversations` survives — the bubble must still be
        // drillable.
        let mut state = AppState::new();
        let bubble_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        state.start_child_run(ChildRunInfo {
            run_id,
            bubble_id,
            agent_type: "reviewer".to_string(),
            task: "look into flaky test".to_string(),
            label: "Investigate flaky test".to_string(),
        });
        state.running_child = None;
        let mouse = spawn_bubble_click_fixture(&mut state, bubble_id);

        update(&mut state, TuiEvent::Mouse(mouse), &empty_textarea());
        state.conversation.chat_drag_active = true;
        state.conversation.chat_selection = Some(ChatSelection {
            anchor_line: 0,
            anchor_col: 3,
            cursor_line: 0,
            cursor_col: 3,
        });
        update(&mut state, TuiEvent::Mouse(mouse), &empty_textarea());

        assert!(matches!(
            state.active_view,
            ConversationView::Child { run_id: r, .. } if r == run_id
        ));
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
    fn domain_stream_done_preserves_manual_scroll_position() {
        // Regression test: a response completing must not override a
        // manual scroll-up (e.g. to view an earlier subagent bubble) by
        // force-snapping the chat back to the bottom.
        let mut state = AppState::new();
        state.mode = TuiMode::Streaming;
        state.conversation.scroll_locked = false;
        state.conversation.scroll_offset = 12;
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::StreamDone),
            &empty_textarea(),
        );
        assert!(!state.conversation.scroll_locked);
        assert_eq!(state.conversation.scroll_offset, 12);
    }

    #[test]
    fn domain_stream_cancelled_preserves_manual_scroll_position() {
        let mut state = AppState::new();
        state.mode = TuiMode::Streaming;
        state.conversation.scroll_locked = false;
        state.conversation.scroll_offset = 12;
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::StreamCancelled),
            &empty_textarea(),
        );
        assert!(!state.conversation.scroll_locked);
        assert_eq!(state.conversation.scroll_offset, 12);
    }

    #[test]
    fn domain_stream_thinking_chunk_appends_to_partial_thinking_text_while_streaming() {
        let mut state = AppState::new();
        state.mode = TuiMode::Streaming;
        state.conversation.scroll_locked = true;
        state.conversation.scroll_offset = 7;
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::StreamThinkingChunk("pondering...".to_string())),
            &empty_textarea(),
        );
        assert_eq!(state.turn_buffer.partial_thinking_text, "pondering...");
        assert!(
            state.turn_buffer.partial_assistant_text.is_empty(),
            "thinking must not contaminate the assistant text accumulator"
        );
        assert_eq!(
            state.conversation.scroll_offset, 0,
            "scroll-locked must snap to bottom"
        );
    }

    #[test]
    fn domain_streaming_interleaved_thinking_and_response_flush_into_separate_bubbles_in_order() {
        // A turn that alternates thinking → response → thinking with no tool
        // call in between must still split into one bubble per segment, in
        // chronological order, rather than merging every thinking fragment
        // into one bubble and every response fragment into another.
        let mut state = AppState::new();
        state.mode = TuiMode::Streaming;
        for event in [
            DomainEvent::StreamThinkingChunk("first thought".to_string()),
            DomainEvent::StreamChunk("first reply".to_string()),
            DomainEvent::StreamThinkingChunk("second thought".to_string()),
            DomainEvent::StreamChunk("second reply".to_string()),
            DomainEvent::StreamDone,
        ] {
            update(&mut state, TuiEvent::Domain(event), &empty_textarea());
        }

        assert_eq!(state.chat_entries.len(), 4, "{:?}", state.chat_entries);
        assert!(matches!(
            &state.chat_entries[0],
            ChatEntry::ThinkingMessage { text, .. } if text == "first thought"
        ));
        assert!(matches!(
            &state.chat_entries[1],
            ChatEntry::AssistantMessage { text, .. } if text == "first reply"
        ));
        assert!(matches!(
            &state.chat_entries[2],
            ChatEntry::ThinkingMessage { text, .. } if text == "second thought"
        ));
        assert!(matches!(
            &state.chat_entries[3],
            ChatEntry::AssistantMessage { text, .. } if text == "second reply"
        ));
    }

    #[test]
    fn child_agent_token_interleaved_thinking_and_response_flush_into_separate_bubbles_in_order() {
        // Mirrors `domain_streaming_interleaved_thinking_and_response_flush_into_separate_bubbles_in_order`
        // for a child conversation: `ChildAgentToken`'s `is_thinking` flag must
        // route each fragment to the child's own `TurnBuffer` the same way the
        // root's `StreamThinkingChunk`/`StreamChunk` do, rather than merging
        // thinking and response text into a single blob (the bug this test
        // guards against).
        let mut state = AppState::new();
        let run_id = Uuid::new_v4();
        state.start_child_run(ChildRunInfo {
            run_id,
            bubble_id: Uuid::new_v4(),
            agent_type: "reviewer".to_string(),
            task: "look into flaky test".to_string(),
            label: "Investigate flaky test".to_string(),
        });
        for (text, is_thinking) in [
            ("first thought", true),
            ("first reply", false),
            ("second thought", true),
            ("second reply", false),
        ] {
            update(
                &mut state,
                TuiEvent::Domain(DomainEvent::ChildAgentToken {
                    run_id,
                    text: text.to_string(),
                    is_thinking,
                }),
                &empty_textarea(),
            );
        }
        state.flush_child_partial_text(run_id);

        let conversation = state
            .child_conversations
            .get(&run_id)
            .expect("conversation");
        assert_eq!(
            conversation.messages.len(),
            4,
            "{:?}",
            conversation.messages
        );
        assert!(matches!(
            &conversation.messages[0],
            ChatEntry::ThinkingMessage { text, .. } if text == "first thought"
        ));
        assert!(matches!(
            &conversation.messages[1],
            ChatEntry::AssistantMessage { text, .. } if text == "first reply"
        ));
        assert!(matches!(
            &conversation.messages[2],
            ChatEntry::ThinkingMessage { text, .. } if text == "second thought"
        ));
        assert!(matches!(
            &conversation.messages[3],
            ChatEntry::AssistantMessage { text, .. } if text == "second reply"
        ));
    }

    #[test]
    fn child_context_usage_is_kept_per_run_and_selected_only_for_drilled_view() {
        let mut state = AppState::new();
        let first_run_id = Uuid::new_v4();
        let second_run_id = Uuid::new_v4();
        for (run_id, label) in [(first_run_id, "First"), (second_run_id, "Second")] {
            state.start_child_run(ChildRunInfo {
                run_id,
                bubble_id: Uuid::new_v4(),
                agent_type: "reviewer".to_string(),
                task: format!("Review {label}"),
                label: label.to_string(),
            });
        }
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ChildContextUsage {
                run_id: first_run_id,
                estimated_tokens: 2_000,
                context_length: 16_000,
                source: ContextUsageSource::SerializedRequest,
            }),
            &empty_textarea(),
        );
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ChildContextUsage {
                run_id: second_run_id,
                estimated_tokens: 6_000,
                context_length: 32_000,
                source: ContextUsageSource::ModelView,
            }),
            &empty_textarea(),
        );

        assert_eq!(
            state.context_usage, None,
            "a background child's report must not overwrite root usage"
        );
        assert_eq!(
            state.active_context_usage(),
            None,
            "root view selects root usage"
        );

        state.switch_to_child_view(first_run_id);
        assert_eq!(
            state.active_context_usage(),
            Some(ContextUsage {
                estimated_tokens: 2_000,
                context_length: 16_000,
                source: ContextUsageSource::SerializedRequest,
            })
        );
        state.switch_to_child_view(second_run_id);
        assert_eq!(
            state.active_context_usage(),
            Some(ContextUsage {
                estimated_tokens: 6_000,
                context_length: 32_000,
                source: ContextUsageSource::ModelView,
            })
        );

        state.switch_to_root_view();
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ContextUsage {
                estimated_tokens: 8_000,
                context_length: 64_000,
                source: ContextUsageSource::ModelView,
            }),
            &empty_textarea(),
        );
        assert_eq!(
            state.active_context_usage(),
            Some(ContextUsage {
                estimated_tokens: 8_000,
                context_length: 64_000,
                source: ContextUsageSource::ModelView,
            }),
            "root view returns to root usage"
        );

        state.switch_to_child_view(first_run_id);
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ChildAgentDone {
                run_id: first_run_id,
                status: "success".to_string(),
                result: Some("done".to_string()),
            }),
            &empty_textarea(),
        );
        assert_eq!(
            state.active_context_usage(),
            Some(ContextUsage {
                estimated_tokens: 2_000,
                context_length: 16_000,
                source: ContextUsageSource::SerializedRequest,
            }),
            "finished child readout remains available while its transcript is drilled in"
        );
    }

    #[test]
    fn compaction_activity_is_scoped_and_preserves_agent_mode() {
        let mut state = AppState::new();
        let root_request_id = Uuid::new_v4();
        let child_run_id = Uuid::new_v4();
        let child_request_id = Uuid::new_v4();
        for (run_id, request_id) in [
            (None, root_request_id),
            (Some(child_run_id), child_request_id),
        ] {
            update(
                &mut state,
                TuiEvent::Domain(DomainEvent::CompactionActivity {
                    run_id,
                    request_id,
                    active: true,
                }),
                &empty_textarea(),
            );
        }
        assert_eq!(state.mode, TuiMode::Idle);
        assert_eq!(state.compaction_requests.len(), 2);
        for request_id in [Uuid::new_v4(), root_request_id] {
            update(
                &mut state,
                TuiEvent::Domain(DomainEvent::CompactionActivity {
                    run_id: None,
                    request_id,
                    active: false,
                }),
                &empty_textarea(),
            );
            if request_id != root_request_id {
                assert_eq!(state.compaction_requests.get(&None), Some(&root_request_id));
            }
        }
        assert!(!state.compaction_requests.contains_key(&None));
        assert_eq!(
            state.compaction_requests.get(&Some(child_run_id)),
            Some(&child_request_id)
        );
        state.clear_conversation();
        assert!(state.compaction_requests.is_empty());
    }

    #[test]
    fn compaction_replaces_the_stale_root_footer_value() {
        let mut state = AppState::new();
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ContextUsage {
                estimated_tokens: 76_915,
                context_length: 98_304,
                source: ContextUsageSource::SerializedRequest,
            }),
            &empty_textarea(),
        );
        assert!(state.context_usage.is_some());

        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ContextUsage {
                estimated_tokens: 9_854,
                context_length: 98_304,
                source: ContextUsageSource::ModelView,
            }),
            &empty_textarea(),
        );

        assert_eq!(
            state.active_context_usage(),
            Some(ContextUsage {
                estimated_tokens: 9_854,
                context_length: 98_304,
                source: ContextUsageSource::ModelView,
            }),
            "post-compaction model-view estimate replaces the stale request estimate"
        );
    }

    #[test]
    fn compaction_usage_update_does_not_interrupt_the_follow_up_response_state() {
        let mut state = AppState::new();
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::StreamDone),
            &empty_textarea(),
        );
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ContextUsage {
                estimated_tokens: 9_854,
                context_length: 98_304,
                source: ContextUsageSource::ModelView,
            }),
            &empty_textarea(),
        );
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::StreamStarted),
            &empty_textarea(),
        );

        assert_eq!(state.mode, TuiMode::Streaming);
        assert_eq!(
            state.active_context_usage().map(|usage| usage.source),
            Some(ContextUsageSource::ModelView)
        );
    }

    #[test]
    fn domain_stream_started_clears_partial_thinking_text() {
        let mut state = AppState::new();
        state.mode = TuiMode::Pending;
        state.turn_buffer.partial_thinking_text = "stale thinking".to_string();
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::StreamStarted),
            &empty_textarea(),
        );
        assert!(state.turn_buffer.partial_thinking_text.is_empty());
    }

    #[test]
    fn domain_tool_start_flushes_pending_thinking_before_new_tool_bubble() {
        // An auto-run tool call (no ToolPending approval step) must flush any
        // thinking accumulated so far into its own bubble *before* the tool
        // bubble is inserted, so the chat log reads in chronological order:
        // "thought, then called the tool" rather than "called the tool, then
        // thought" (the latter happened when thinking was only flushed on
        // StreamDone, at the very end of the whole tool-using turn).
        let mut state = AppState::new();
        state.mode = TuiMode::Streaming;
        state.turn_buffer.partial_thinking_text = "deciding to read the file".to_string();
        state.turn_buffer.thinking_started_at = Some(std::time::Instant::now());
        let tool_id = Uuid::new_v4();
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolStart {
                id: tool_id,
                name: "read".to_string(),
                arguments: serde_json::json!({}),
                content_sections: vec![],
                title_arguments: vec![],
                run_id: None,
            }),
            &empty_textarea(),
        );

        assert!(
            state.turn_buffer.partial_thinking_text.is_empty(),
            "thinking must be flushed, not left dangling"
        );
        assert_eq!(
            state.chat_entries.len(),
            2,
            "expected [ThinkingMessage, ToolBubble]"
        );
        assert!(matches!(
            &state.chat_entries[0],
            ChatEntry::ThinkingMessage { text, .. } if text == "deciding to read the file"
        ));
        assert!(matches!(
            &state.chat_entries[1],
            ChatEntry::ToolBubble { id, .. } if *id == tool_id
        ));
    }

    #[test]
    fn live_read_tool_bubble_matches_serialized_and_replayed_bubble() {
        let result = "# Ylva\nThe Agent Harness.".to_string();
        let content_sections = vec![Section::Content(crate::agent::tools::ContentSection {
            label: None,
            content: result.clone(),
            file_extension: Some("md".to_string()),
            line_number_offset: Some(0),
        })];
        assert_tool_bubble_roundtrip(ToolBubbleRoundtripCase {
            tool_id: Uuid::new_v4(),
            tool_name: "read".to_string(),
            arguments: serde_json::json!({"path": "README.md"}),
            initial_content_sections: vec![],
            title_arguments: vec![(
                "path".to_string(),
                serde_json::Value::String("README.md".to_string()),
            )],
            result: result.clone(),
            result_metadata: None,
            result_metadata_schema: None,
            result_section: ResultSection::plain(result),
            summary: vec![
                ("read_bytes".to_string(), "28".to_string()),
                ("read_lines".to_string(), "1".to_string()),
            ],
            content_sections,
            duration: Duration::from_millis(37),
        });
    }

    #[test]
    fn live_write_tool_bubble_matches_serialized_and_replayed_bubble() {
        let path = "src/example.rs";
        let content = "fn main() {\n    println!(\"hello\");\n}\n".to_string();
        let content_sections = vec![Section::Content(crate::agent::tools::ContentSection {
            label: None,
            content: content.clone(),
            file_extension: Some("rs".to_string()),
            line_number_offset: Some(0),
        })];
        let bytes_written = content.len();
        let result = format!("wrote {bytes_written} bytes to '{path}'");

        assert_tool_bubble_roundtrip(ToolBubbleRoundtripCase {
            tool_id: Uuid::new_v4(),
            tool_name: "write".to_string(),
            arguments: serde_json::json!({
                "path": path,
                "content": content,
            }),
            initial_content_sections: content_sections.clone(),
            title_arguments: vec![(
                "path".to_string(),
                serde_json::Value::String(path.to_string()),
            )],
            result: result.clone(),
            result_metadata: Some(serde_json::json!({"bytes_written": bytes_written})),
            result_metadata_schema: None,
            result_section: ResultSection::plain(result),
            summary: vec![("bytes_written".to_string(), bytes_written.to_string())],
            content_sections,
            duration: Duration::from_millis(42),
        });
    }

    #[test]
    fn write_error_bubble_matches_real_summary_fixture_after_resume() {
        let content = r#"### Conversation Summary

**Sky Blue:** Attributed to **Rayleigh scattering**, where atmospheric gas molecules scatter short-wavelength blue and violet light more effectively than longer wavelengths.
"#;
        assert_tool_error_bubble_roundtrip(ToolErrorBubbleRoundtripCase {
            tool_id: Uuid::parse_str("c15523ca-6b84-4ffb-969c-9531d47a768a")
                .expect("real write call id"),
            tool_name: "write".to_string(),
            arguments: serde_json::json!({
                "content": content,
                "path": "../logs/summary.md",
            }),
            initial_content_sections: vec![],
            title_arguments: vec![(
                "path".to_string(),
                serde_json::Value::String("../logs/summary.md".to_string()),
            )],
            error: "Access denied by security policy: path is outside the workspace root; only paths within the workspace are accessible"
                .to_string(),
            duration: Duration::ZERO,
            content_sections: vec![],
        });
    }

    #[test]
    fn live_bash_tool_bubble_matches_serialized_and_replayed_bubble() {
        let result = "hello from bash\n".to_string();
        assert_tool_bubble_roundtrip(ToolBubbleRoundtripCase {
            tool_id: Uuid::new_v4(),
            tool_name: "bash".to_string(),
            arguments: serde_json::json!({"command": "printf 'hello from bash\\n'"}),
            initial_content_sections: vec![],
            title_arguments: vec![(
                "command".to_string(),
                serde_json::Value::String("printf 'hello from bash\\n'".to_string()),
            )],
            result: result.clone(),
            result_metadata: Some(serde_json::json!({
                "output_bytes": result.len(),
            })),
            result_metadata_schema: None,
            result_section: ResultSection::plain(result.clone()),
            summary: vec![("output_bytes".to_string(), result.len().to_string())],
            content_sections: vec![Section::Content(ContentSection {
                label: Some("Result".to_string()),
                content: result,
                file_extension: None,
                line_number_offset: Some(0),
            })],
            duration: Duration::from_millis(51),
        });
    }

    #[test]
    fn live_glob_tool_bubble_matches_serialized_and_replayed_bubble() {
        let result = "src/lib.rs\nsrc/main.rs".to_string();
        assert_tool_bubble_roundtrip(ToolBubbleRoundtripCase {
            tool_id: Uuid::new_v4(),
            tool_name: "glob".to_string(),
            arguments: serde_json::json!({"pattern": "src/**/*.rs"}),
            initial_content_sections: vec![],
            title_arguments: vec![(
                "pattern".to_string(),
                serde_json::Value::String("src/**/*.rs".to_string()),
            )],
            result: result.clone(),
            result_metadata: Some(serde_json::json!({
                "matches": 2,
            })),
            result_metadata_schema: None,
            result_section: ResultSection::plain(result.clone()),
            summary: vec![("matches".to_string(), "2".to_string())],
            content_sections: vec![Section::Content(ContentSection {
                label: None,
                content: result,
                file_extension: None,
                line_number_offset: Some(0),
            })],
            duration: Duration::from_millis(23),
        });
    }

    #[test]
    fn live_search_tool_bubble_matches_serialized_and_replayed_bubble() {
        let result = "src/lib.rs:10: fn main()\nsrc/lib.rs-9- context".to_string();
        assert_tool_bubble_roundtrip(ToolBubbleRoundtripCase {
            tool_id: Uuid::new_v4(),
            tool_name: "search".to_string(),
            arguments: serde_json::json!({
                "pattern": "main",
                "path": "src",
                "glob": "**/*.rs",
                "context_lines": 1,
            }),
            initial_content_sections: vec![],
            title_arguments: vec![
                (
                    "pattern".to_string(),
                    serde_json::Value::String("main".to_string()),
                ),
                (
                    "path".to_string(),
                    serde_json::Value::String("src".to_string()),
                ),
                (
                    "glob".to_string(),
                    serde_json::Value::String("**/*.rs".to_string()),
                ),
                ("context_lines".to_string(), serde_json::json!(1)),
            ],
            result: result.clone(),
            result_metadata: Some(serde_json::json!({
                "matching_files": 1,
                "matching_lines": 1,
            })),
            result_metadata_schema: None,
            result_section: ResultSection::plain(result),
            summary: vec![
                ("matching_files".to_string(), "1".to_string()),
                ("matching_lines".to_string(), "1".to_string()),
            ],
            content_sections: vec![Section::SearchResult(SearchResultSection {
                file_path: "src/lib.rs".to_string(),
                file_extension: Some("rs".to_string()),
                lines: vec![
                    SearchResultLine {
                        line_number: 9,
                        is_match: false,
                        content: "context".to_string(),
                    },
                    SearchResultLine {
                        line_number: 10,
                        is_match: true,
                        content: "fn main()".to_string(),
                    },
                ],
            })],
            duration: Duration::from_millis(64),
        });
    }

    #[test]
    fn live_edit_tool_bubble_matches_serialized_and_replayed_bubble() {
        let path = "src/example.rs";
        let old_string = "old()";
        let new_string = "new()";
        let diff = "@@ -1 +1 @@\n-old()\n+new()\n".to_string();
        let result = format!("Edited '{path}': 1 replacement(s) made");
        assert_tool_bubble_roundtrip(ToolBubbleRoundtripCase {
            tool_id: Uuid::new_v4(),
            tool_name: "edit".to_string(),
            arguments: serde_json::json!({
                "path": path,
                "mode": "search_replace",
                "old_string": old_string,
                "new_string": new_string,
                "replace_all": false,
            }),
            initial_content_sections: vec![Section::Diff(DiffSection {
                label: Some(path.to_string()),
                content: diff.clone(),
            })],
            title_arguments: vec![
                (
                    "path".to_string(),
                    serde_json::Value::String(path.to_string()),
                ),
                (
                    "mode".to_string(),
                    serde_json::Value::String("search_replace".to_string()),
                ),
                ("replace_all".to_string(), serde_json::Value::Bool(false)),
            ],
            result: result.clone(),
            result_metadata: Some(serde_json::json!({
                "replacements_made": 1,
            })),
            result_metadata_schema: None,
            result_section: ResultSection::plain(result),
            summary: vec![("replacements_made".to_string(), "1".to_string())],
            content_sections: vec![Section::Diff(DiffSection {
                label: Some(path.to_string()),
                content: diff,
            })],
            duration: Duration::from_millis(48),
        });
    }

    #[test]
    fn live_multiline_clarify_fixture_matches_after_resume() {
        let tool_id =
            Uuid::parse_str("5c232376-718f-4e84-bbd6-6042d6dfb13c").expect("fixture clarify id");
        let question =
            "Would you like to replace the 'lamp' in the poem with a different animal? Which animal should I use?";
        let context = "The current poem is:\n\nA small lamp glows so bright,\nA tiny beacon in the night.\nIt chases shadows with a spark,\nAnd banishes the dark.";
        let choices = vec![
            "Tiger".to_string(),
            "Elephant".to_string(),
            "Fox".to_string(),
        ];
        let content_sections = vec![
            Section::Content(ContentSection {
                label: Some("Context".to_string()),
                content: context.to_string(),
                file_extension: None,
                line_number_offset: None,
            }),
            Section::Content(ContentSection {
                label: Some("Choices".to_string()),
                content: "1. Tiger\n2. Elephant\n3. Fox".to_string(),
                file_extension: None,
                line_number_offset: None,
            }),
        ];
        let duration = Duration::new(8, 90_442_083);
        // A fresh session persists only the question in title_arguments;
        // context and choices are the completed bubble's body sections.
        let title_arguments = vec![(
            "question".to_string(),
            serde_json::Value::String(question.to_string()),
        )];

        let mut live_state = AppState::new();
        live_state.mode = TuiMode::Streaming;
        update(
            &mut live_state,
            TuiEvent::Domain(DomainEvent::ClarifyPending {
                id: tool_id,
                question: question.to_string(),
                context: Some(context.to_string()),
                choices: Some(choices.clone()),
                run_id: None,
            }),
            &empty_textarea(),
        );
        update(
            &mut live_state,
            TuiEvent::Domain(DomainEvent::ToolDone {
                id: tool_id,
                name: "clarify".to_string(),
                result_section: ResultSection {
                    id: crate::tui_reducer_state::result_section_ids::USER_ANSWER.to_string(),
                    content: "Elephant".to_string(),
                },
                summary: vec![],
                duration,
                content_sections: content_sections.clone(),
                run_id: None,
            }),
            &empty_textarea(),
        );
        update(
            &mut live_state,
            TuiEvent::Domain(DomainEvent::StreamStarted),
            &empty_textarea(),
        );

        let persisted_messages = [
            Message::Assistant {
                content: String::new(),
                status: AssistantStatus::Complete,
                tool_calls: vec![ToolCallRequest {
                    id: tool_id,
                    name: "clarify".to_string(),
                    arguments: serde_json::json!({
                        "choices": choices,
                        "context": context,
                        "question": question,
                    }),
                    persisted_title_arguments: title_arguments,
                }],
            },
            Message::ToolResult {
                value: ToolResult {
                    id: tool_id,
                    name: "clarify".to_string(),
                    result: "Elephant".to_string(),
                    attachments: vec![],
                    result_section_id: Some(
                        crate::tui_reducer_state::result_section_ids::USER_ANSWER.to_string(),
                    ),
                    result_metadata: None,
                    result_metadata_schema: None,
                    summary: vec![],
                    content_sections,
                    duration,
                },
            },
        ];
        let replayed_messages = persisted_messages
            .iter()
            .map(|message| {
                let serialized = serde_json::to_string(message).expect("message serializes");
                serde_json::from_str::<Message>(&serialized).expect("message deserializes")
            })
            .collect::<Vec<_>>();
        let mut replayed_state = AppState::new();
        replayed_state.replay_messages(&replayed_messages, &[], None);
        let replayed_entries = replayed_state.chat_entries.clone();

        live_state.mode = TuiMode::Idle;
        replayed_state.mode = TuiMode::Idle;
        let live_buffer = render_app_state_buffer(&mut live_state);
        let replayed_buffer = render_app_state_buffer(&mut replayed_state);

        assert_eq!(live_state.chat_entries, replayed_entries);
        assert_eq!(live_buffer, replayed_buffer);
    }

    #[test]
    fn live_agent_finish_tool_bubble_matches_serialized_and_replayed_bubble() {
        let result = "The child completed the requested inspection.".to_string();
        assert_tool_bubble_roundtrip(ToolBubbleRoundtripCase {
            tool_id: Uuid::new_v4(),
            tool_name: "agent_finish".to_string(),
            arguments: serde_json::json!({
                "status": "success",
                "result": result,
                "sources": ["src/lib.rs"],
            }),
            initial_content_sections: vec![],
            title_arguments: vec![
                (
                    "status".to_string(),
                    serde_json::Value::String("success".to_string()),
                ),
                (
                    "result".to_string(),
                    serde_json::Value::String(result.clone()),
                ),
                ("sources".to_string(), serde_json::json!(["src/lib.rs"])),
            ],
            result: "Reported success to the parent agent.".to_string(),
            result_metadata: None,
            result_metadata_schema: None,
            result_section: ResultSection::plain("Reported success to the parent agent."),
            summary: vec![],
            content_sections: vec![],
            duration: Duration::from_millis(19),
        });
    }

    #[test]
    fn live_spawn_subagent_tool_bubble_matches_serialized_and_replayed_bubble() {
        let task = "Inspect the parser and report any unsafe assumptions.";
        let briefing = "Focus on input validation and error propagation.";
        let child_result = "The parser validates all external input.";
        let sources = vec!["src/parser.rs".to_string()];
        let run_id = Uuid::new_v4();
        let call_id = Uuid::new_v4();
        let task_section = Section::Content(ContentSection {
            label: Some("Task".to_string()),
            content: task.to_string(),
            file_extension: Some("md".to_string()),
            line_number_offset: None,
        });
        let briefing_section = Section::Content(ContentSection {
            label: Some("Briefing".to_string()),
            content: briefing.to_string(),
            file_extension: Some("md".to_string()),
            line_number_offset: None,
        });
        let result_section = Section::Content(ContentSection {
            label: Some("Result".to_string()),
            content: child_result.to_string(),
            file_extension: Some("md".to_string()),
            line_number_offset: None,
        });
        let sources_section = Section::Content(ContentSection {
            label: Some("Sources".to_string()),
            content: "- src/parser.rs".to_string(),
            file_extension: Some("md".to_string()),
            line_number_offset: None,
        });
        let final_content_sections = vec![
            task_section.clone(),
            briefing_section.clone(),
            result_section,
            sources_section,
        ];

        assert_tool_bubble_roundtrip_with_child_runs(
            ToolBubbleRoundtripCase {
                tool_id: call_id,
                tool_name: "spawn_subagent".to_string(),
                arguments: serde_json::json!({
                    "label": "Inspect parser safety",
                    "task": task,
                    "briefing": briefing,
                }),
                initial_content_sections: vec![task_section, briefing_section],
                title_arguments: vec![(
                    "label".to_string(),
                    serde_json::Value::String("Inspect parser safety".to_string()),
                )],
                result: serde_json::to_string(&serde_json::json!({
                    "status": "running",
                    "run_id": run_id,
                }))
                .expect("spawn result serializes"),
                result_metadata: None,
                result_metadata_schema: None,
                result_section: ResultSection::plain(""),
                summary: vec![("status".to_string(), "success".to_string())],
                content_sections: final_content_sections,
                duration: Duration::from_millis(91),
            },
            &[crate::store::ChildRunRecord {
                run_id,
                call_id,
                parent_session: Uuid::new_v4(),
                agent_type: "default".to_string(),
                task: task.to_string(),
                label: "Inspect parser safety".to_string(),
                created_at: "2026-09-11T07:00:00.000Z".to_string(),
                completed_at: Some("2026-09-11T07:00:00.091Z".to_string()),
                status: Some("success".to_string()),
                result: Some(child_result.to_string()),
                sources,
                origin: crate::child_agent_manager::FinishResultOrigin::Agent,
                duration: Some(Duration::from_millis(91)),
            }],
        );
    }

    #[test]
    fn domain_tool_progress_patches_progress_arguments_on_running_bubble() {
        // `ToolProgress` is an interim status update (e.g. `spawn_subagent`
        // reporting status=spawned/queued) — it must patch `progress_arguments`
        // on the existing `Running` bubble (not `title_arguments`, which stays
        // the call's real, static arguments) without transitioning it to a
        // terminal state (spinner keeps going, content sections untouched).
        let mut state = AppState::new();
        state.mode = TuiMode::Streaming;
        let tool_id = Uuid::new_v4();
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolStart {
                id: tool_id,
                name: "spawn_subagent".to_string(),
                arguments: serde_json::json!({}),
                content_sections: vec![],
                title_arguments: vec![(
                    "label".to_string(),
                    serde_json::json!("Refactor auth token validation"),
                )],
                run_id: None,
            }),
            &empty_textarea(),
        );

        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolProgress {
                id: tool_id,
                title_arguments: vec![
                    ("status".to_string(), serde_json::json!("running")),
                    ("run_id".to_string(), serde_json::json!("abc-123")),
                ],
            }),
            &empty_textarea(),
        );

        let entry = state
            .chat_entries
            .iter()
            .find(|entry| matches!(entry, ChatEntry::ToolBubble { id, .. } if *id == tool_id))
            .expect("tool bubble should still exist");
        match entry {
            ChatEntry::ToolBubble { state: bubble, .. } => match bubble.as_ref() {
                ToolCallState::Running {
                    title_arguments,
                    progress_arguments,
                    ..
                } => {
                    assert_eq!(
                        title_arguments,
                        &vec![(
                            "label".to_string(),
                            serde_json::json!("Refactor auth token validation")
                        )],
                        "the call's real title_arguments must be untouched by ToolProgress"
                    );
                    assert_eq!(
                        progress_arguments,
                        &vec![
                            ("status".to_string(), serde_json::json!("running")),
                            ("run_id".to_string(), serde_json::json!("abc-123")),
                        ]
                    );
                }
                other => panic!("expected the bubble to remain Running, got: {other:?}"),
            },
            _ => unreachable!(),
        }
    }

    #[test]
    fn domain_tool_done_does_not_inherit_stale_progress_arguments() {
        // Regression test: once ToolDone arrives, any interim
        // `progress_arguments` (e.g. status=spawned/run_id=...) must not
        // linger in the terminal bubble — only the call's real
        // `title_arguments` (e.g. `label`) survive, and the final status
        // comes solely from `summary`.
        let mut state = AppState::new();
        state.mode = TuiMode::Streaming;
        let tool_id = Uuid::new_v4();
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolStart {
                id: tool_id,
                name: "spawn_subagent".to_string(),
                arguments: serde_json::json!({}),
                content_sections: vec![],
                title_arguments: vec![(
                    "label".to_string(),
                    serde_json::json!("Refactor auth token validation"),
                )],
                run_id: None,
            }),
            &empty_textarea(),
        );
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolProgress {
                id: tool_id,
                title_arguments: vec![
                    ("status".to_string(), serde_json::json!("running")),
                    ("run_id".to_string(), serde_json::json!("abc-123")),
                ],
            }),
            &empty_textarea(),
        );
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolDone {
                id: tool_id,
                name: "spawn_subagent".to_string(),
                result_section: crate::tui_reducer_state::ResultSection::plain(String::new()),
                content_sections: vec![],
                summary: vec![("status".to_string(), "unstructured".to_string())],
                duration: std::time::Duration::from_millis(1500),
                run_id: None,
            }),
            &empty_textarea(),
        );

        let entry = state
            .chat_entries
            .iter()
            .find(|entry| matches!(entry, ChatEntry::ToolBubble { id, .. } if *id == tool_id))
            .expect("tool bubble should still exist");
        match entry {
            ChatEntry::ToolBubble { state: bubble, .. } => match bubble.as_ref() {
                ToolCallState::Done {
                    title_arguments,
                    summary,
                    duration,
                    ..
                } => {
                    assert_eq!(
                        title_arguments,
                        &vec![(
                            "label".to_string(),
                            serde_json::json!("Refactor auth token validation")
                        )],
                        "the stale spawned/run_id progress values must not leak into Done"
                    );
                    assert_eq!(
                        summary,
                        &vec![("status".to_string(), "unstructured".to_string())]
                    );
                    assert_eq!(*duration, std::time::Duration::from_millis(1500));
                }
                other => panic!("expected the bubble to be Done, got: {other:?}"),
            },
            _ => unreachable!(),
        }
    }

    #[test]
    fn domain_tool_progress_is_a_no_op_when_bubble_is_not_running() {
        // Once a bubble reached a terminal state, a stray/late ToolProgress
        // must not resurrect or otherwise mutate it.
        let mut state = AppState::new();
        state.mode = TuiMode::Streaming;
        let tool_id = Uuid::new_v4();
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolStart {
                id: tool_id,
                name: "spawn_subagent".to_string(),
                arguments: serde_json::json!({}),
                content_sections: vec![],
                title_arguments: vec![],
                run_id: None,
            }),
            &empty_textarea(),
        );
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolDone {
                id: tool_id,
                name: "spawn_subagent".to_string(),
                result_section: crate::tui_reducer_state::ResultSection::plain(String::new()),
                content_sections: vec![],
                summary: vec![("status".to_string(), "success".to_string())],
                duration: std::time::Duration::ZERO,
                run_id: None,
            }),
            &empty_textarea(),
        );

        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolProgress {
                id: tool_id,
                title_arguments: vec![("status".to_string(), serde_json::json!("running"))],
            }),
            &empty_textarea(),
        );

        let entry = state
            .chat_entries
            .iter()
            .find(|entry| matches!(entry, ChatEntry::ToolBubble { id, .. } if *id == tool_id))
            .expect("tool bubble should still exist");
        match entry {
            ChatEntry::ToolBubble { state: bubble, .. } => {
                assert!(
                    matches!(bubble.as_ref(), ToolCallState::Done { .. }),
                    "a late ToolProgress must not move the bubble out of Done, got: {bubble:?}"
                );
            }
            _ => unreachable!(),
        }
    }

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
                run_id: None,
            }),
            &empty_textarea(),
        );
        assert!(effects.contains(&Effect::DenyTool {
            id: tool_id,
            reason: String::new(),
        }));
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
    fn tool_pending_parks_the_input_state_via_the_save_effect() {
        // Regression: a half-typed slash command left the suggestion overlay
        // open when a tool approval arrived, and the approval footer draws on
        // the same rows above the input. Arming the approval must emit the
        // save effect that parks draft + overlay together and hides the
        // overlay (the snapshot/hide itself happens when the Driver executes
        // the effect — covered by the `tui_driver` tests).
        let mut state = AppState::new();
        state.refresh_slash_overlay("/mo");
        assert!(
            matches!(state.slash_overlay, SlashOverlay::Suggestions { .. }),
            "expected the suggestion overlay to be open, got {:?}",
            state.slash_overlay
        );

        let effects = update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolPending {
                id: Uuid::new_v4(),
                name: "write".to_string(),
                arguments: serde_json::json!({}),
                content_sections: vec![],
                title_arguments: vec![],
                run_id: None,
            }),
            &textarea_with("/mo"),
        );
        assert!(matches!(state.focus, Focus::ToolApproval { .. }));
        assert!(
            effects.contains(&Effect::SaveInputStateAndClear),
            "expected the input-state save effect, got {effects:?}"
        );
    }

    #[test]
    fn closing_help_during_an_outstanding_approval_returns_to_the_approval_focus() {
        // Regression: closing the help popup used to drop focus to
        // `Focus::MainInput` even while a tool still awaited its verdict —
        // handing the slash-command-capable main input to the user and
        // hiding the approval controls.
        let mut state = AppState::new();
        let tool_id = Uuid::new_v4();
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolPending {
                id: tool_id,
                name: "write".to_string(),
                arguments: serde_json::json!({}),
                content_sections: vec![],
                title_arguments: vec![],
                run_id: None,
            }),
            &empty_textarea(),
        );
        assert!(matches!(state.focus, Focus::ToolApproval { .. }));

        let effects = update(
            &mut state,
            TuiEvent::Input(InputEvent::ShowHelp),
            &empty_textarea(),
        );
        assert!(effects.is_empty());
        assert!(matches!(state.focus, Focus::HelpPopup));

        let effects = update(
            &mut state,
            TuiEvent::Input(InputEvent::Escape),
            &empty_textarea(),
        );
        assert_eq!(
            state.focus,
            Focus::ToolApproval {
                tool_id,
                tool_name: "write".to_string()
            },
            "closing help must resume the approval focus while the verdict is outstanding"
        );
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, Effect::DenyTool { .. })),
            "closing help must not resolve the approval"
        );
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, Effect::RestoreInputState)),
            "closing help must not restore the input — the pause is still active"
        );
    }

    #[test]
    fn confirm_expired_event_clears_double_press_abort_mode() {
        let mut state = AppState::new();
        state.mode = TuiMode::DoublePressAbort {
            pressed_at: Instant::now(),
            return_to: Box::new(TuiMode::Streaming),
            armed_by: ConfirmKey::CtrlC,
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
            armed_by: ConfirmKey::CtrlC,
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
            state.turn_buffer.partial_assistant_text, "hello",
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
                run_id: None,
            }),
            &empty_textarea(),
        );
        update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolDenied { id: tool_id }),
            &empty_textarea(),
        );
        let denied_bubble = state.chat_entries.iter().find_map(|entry| {
            let ChatEntry::ToolBubble { state, .. } = entry else {
                return None;
            };
            if let ToolCallState::Denied { arguments, .. } = state.as_ref() {
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
