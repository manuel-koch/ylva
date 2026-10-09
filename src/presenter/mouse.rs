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

//! Pure mouse event handling.

use super::TextareaContext;
use crate::markdown::render_markdown;
use crate::tui_reducer_state::{
    model_picker_row_entries, slash_overlay_text_rows, AppState, ChatEntry, ChatSelection, Effect,
    Focus, SlashOverlay, TuiMode,
};
use crossterm::event::MouseEventKind;

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
// ─── Mouse routing ────────────────────────────────────────────────────────────

pub(super) fn route_mouse(
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
            } else if matches!(
                state.slash_overlay,
                SlashOverlay::ModelPicker { .. } | SlashOverlay::SessionPicker(_)
            ) && !matches!(state.focus, Focus::ToolApproval { .. })
            {
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
                if let SlashOverlay::SessionPicker(picker) = &mut state.slash_overlay {
                    if picker.phase == crate::tui_reducer_state::SessionPickerPhase::Ready {
                        if state
                            .layout
                            .slash_overlay
                            .resume_action
                            .contains((column, row).into())
                        {
                            picker.action = crate::tui_reducer_state::SessionAction::Resume;
                        } else if state
                            .layout
                            .slash_overlay
                            .clone_action
                            .contains((column, row).into())
                        {
                            picker.action = crate::tui_reducer_state::SessionAction::Clone;
                        } else if let Some(index) = state.layout.slash_overlay.row_at(column, row) {
                            if index < picker.choices.len() {
                                picker.selected = index;
                            }
                        }
                    }
                    return vec![];
                }
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
