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

//! Pure keyboard event handling.

use super::TextareaContext;
use crate::agent::AgentMode;
use crate::slash_command::{parse_slash_command_invocation, SlashCommand, SlashCommandInvocation};
use crate::tui_reducer_state::{
    AppState, ConfirmKey, ConversationView, Effect, Focus, HistoryDirection, InputEvent,
    SlashOverlay, TuiMode, UserMessageState, HELP_HORIZONTAL_SCROLL_COLUMNS,
};
use crossterm::event::{KeyCode, KeyModifiers};
use std::time::Instant;

// ─── Keyboard routing ─────────────────────────────────────────────────────────

pub(super) fn route_keyboard(
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
        InputEvent::StopSignal => handle_stop_signal(state),
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
        InputEvent::StopSignal => handle_stop_signal(state),
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
    if matches!(state.slash_overlay, SlashOverlay::SessionPicker(_)) {
        return keyboard_session_picker(state, event);
    }
    match event {
        InputEvent::ShowHelp => {
            state.focus = Focus::HelpPopup;
            state.clear_help_selection();
            vec![]
        }
        InputEvent::StopSignal => handle_stop_signal(state),
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
        InputEvent::StopSignal => handle_stop_signal(state),
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

/// External SIGINT/SIGTERM, whatever the focus. Idle quits at once. A run in
/// progress (including a live subagent) is stopped, and the TUI quits when the
/// agent reports it idle or killed (`domain_aborting`).
fn handle_stop_signal(state: &mut AppState) -> Vec<Effect> {
    if matches!(state.agent_mode(), AgentMode::Idle) {
        return vec![Effect::Quit];
    }
    tracing::info!("external stop signal: entering Aborting, stop requested");
    state.mode = TuiMode::Aborting;
    state.aborting_exit = true;
    vec![Effect::StopAgent]
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
    // The parameter popup is the innermost layer of the model picker: Esc
    // returns to the model list and keeps the draft, discarding only the
    // popup's edits.
    if let SlashOverlay::ModelPicker {
        parameters: Some(_),
        ..
    } = state.slash_overlay
    {
        state.leave_model_parameters();
        return vec![];
    }
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
fn slash_command_effects(state: &mut AppState, invocation: SlashCommandInvocation) -> Vec<Effect> {
    if matches!(state.active_view, ConversationView::Child { .. })
        && invocation.command == SlashCommand::Compact
    {
        state.push_notice("Manual compaction is available only in the root conversation.".into());
        return vec![Effect::ClearTextarea];
    }
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
        SlashCommand::Session => vec![Effect::ClearTextarea, Effect::ListSessions],
        SlashCommand::Compact => vec![Effect::ClearTextarea, Effect::Compact],
    }
}

fn keyboard_session_picker(state: &mut AppState, event: InputEvent) -> Vec<Effect> {
    match event {
        InputEvent::Escape => {
            state.slash_overlay = SlashOverlay::Hidden;
            vec![Effect::CancelSessionSelection, Effect::ClearTextarea]
        }
        InputEvent::StopSignal => handle_stop_signal(state),
        InputEvent::CtrlC => handle_ctrl_c_generic(state, true),
        InputEvent::ShowHelp => {
            state.focus = Focus::HelpPopup;
            vec![]
        }
        InputEvent::KeyInput(key) => {
            let page_entry_count = state.layout.slash_overlay.page_entry_count();
            let SlashOverlay::SessionPicker(picker) = &mut state.slash_overlay else {
                return vec![];
            };
            if picker.phase != crate::tui_reducer_state::SessionPickerPhase::Ready {
                return vec![];
            }
            match key.code {
                KeyCode::Up => picker.move_selection(-1),
                KeyCode::Down => picker.move_selection(1),
                KeyCode::PageUp => picker.move_selection(-page_entry_count),
                KeyCode::PageDown => picker.move_selection(page_entry_count),
                KeyCode::Left => picker.action = crate::tui_reducer_state::SessionAction::Resume,
                KeyCode::Right => picker.action = crate::tui_reducer_state::SessionAction::Clone,
                KeyCode::Enter if key.modifiers.is_empty() => {
                    if let Some(choice) = picker.selected() {
                        return vec![Effect::SelectSession {
                            source_id: choice.session_id,
                            action: picker.action,
                        }];
                    }
                }
                _ => {}
            }
            vec![]
        }
        InputEvent::Paste(_) => vec![],
    }
}

/// Keys while the `/models` picker is open. The list selects and opens the
/// parameter popup, and typing falls through to the draft so it narrows the
/// list. The popup owns every key, so the filter cannot change under it.
/// Returns `None` when the key is left to the normal draft handling.
fn handle_model_picker_key(state: &mut AppState, code: KeyCode) -> Option<Vec<Effect>> {
    let parameters_open = matches!(
        state.slash_overlay,
        SlashOverlay::ModelPicker {
            parameters: Some(_),
            ..
        }
    );
    if parameters_open {
        match code {
            KeyCode::Up => state.move_slash_selection(-1),
            KeyCode::Down => state.move_slash_selection(1),
            KeyCode::Left => state.adjust_model_parameter(-1),
            KeyCode::Right => state.adjust_model_parameter(1),
            KeyCode::Enter => {
                state.confirm_model_parameters();
                return Some(switch_to_highlighted_model(state));
            }
            _ => {}
        }
        return Some(vec![]);
    }
    match code {
        KeyCode::Up => state.move_slash_selection(-1),
        KeyCode::Down => state.move_slash_selection(1),
        KeyCode::PageUp => {
            let page_entry_count = state.layout.slash_overlay.page_entry_count();
            state.move_slash_selection(-page_entry_count);
        }
        KeyCode::PageDown => {
            let page_entry_count = state.layout.slash_overlay.page_entry_count();
            state.move_slash_selection(page_entry_count);
        }
        KeyCode::Enter => state.open_model_parameters(),
        // Left/right have no meaning in the list; they must not reach the draft.
        KeyCode::Left | KeyCode::Right => {}
        _ => return None,
    }
    Some(vec![])
}

/// Close the picker and switch to the highlighted choice, applying any
/// parameters confirmed in the popup.
fn switch_to_highlighted_model(state: &mut AppState) -> Vec<Effect> {
    let picked = state
        .selected_model()
        .map(|choice| choice.to_model_selection());
    state.slash_overlay = SlashOverlay::Hidden;
    let mut effects = vec![Effect::ClearTextarea];
    if let Some(model) = picked {
        effects.push(Effect::SwitchModel(model));
    }
    effects
}

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
                    return slash_command_effects(
                        state,
                        SlashCommandInvocation {
                            command,
                            argument: None,
                        },
                    );
                }
            }
            _ => {}
        }
    }

    // The model picker owns navigation, reasoning-effort selection, and
    // confirmation while it is open; every other key (including plain
    // typing) falls through to the normal input handling.
    if matches!(state.slash_overlay, SlashOverlay::ModelPicker { .. }) {
        if let Some(effects) = handle_model_picker_key(state, key_event.code) {
            return effects;
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
            if let Some(invocation) = parse_slash_command_invocation(&textarea.text) {
                if matches!(
                    invocation.command,
                    SlashCommand::Session | SlashCommand::Compact
                ) {
                    state.slash_overlay = SlashOverlay::Hidden;
                    return slash_command_effects(state, invocation);
                }
            }
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
                return slash_command_effects(state, invocation);
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
