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

//! View layer — `render()` entry point for the Ylva TUI.
//!
//! This module is excluded from the coverage gate (it needs a live terminal);
//! visual correctness is verified manually.
//!
//! `render` owns the projection from [`AppState`] to the legacy [`TuiState`]
//! snapshot consumed by [`crate::ui::draw`], and syncs the view-geometry
//! fields written back by `ui::draw` into `AppState` after each frame.

use ratatui::Frame;
use ratatui_textarea::TextArea;

use crate::app_state::{AppState, BannerDragSelection, Focus, TuiState};
use crate::ui::{self, ButtonAreas};

/// Draw one TUI frame.
///
/// Projects `state` into a [`TuiState`] snapshot, delegates all rendering to
/// [`crate::ui::draw`], then syncs the view-geometry fields written by
/// `ui::draw` back into `state.layout` and the scroll/render-cache fields.
///
/// Called inside `terminal.draw(|frame| { render::render(frame, …); })`.
pub fn render(
    frame: &mut Frame,
    state: &mut AppState,
    textarea: &mut TextArea<'_>,
    hint: Option<&str>,
    mouse_pos: Option<(u16, u16)>,
) {
    let mut render_state = project_for_draw(state);
    let button_areas = ui::draw(frame, &mut render_state, textarea, hint, mouse_pos);
    sync_from_draw(state, render_state, button_areas);
}

/// Project the canonical [`AppState`] to the legacy [`TuiState`] snapshot
/// required by [`crate::ui::draw`].
///
/// The projection is a shallow clone of all view-model fields. Driver-owned
/// types (`TextArea`, channel handles) are not part of this snapshot.
fn project_for_draw(state: &AppState) -> TuiState {
    let pending_approval = if let Focus::ToolApproval {
        tool_id,
        ref tool_name,
    } = state.focus
    {
        Some((tool_id, tool_name.clone(), serde_json::Value::Null))
    } else {
        None
    };
    TuiState {
        agent_mode: state.agent_mode(),
        chat_entries: state.chat_entries.clone(),
        partial_assistant_text: state.partial_assistant_text.clone(),
        show_help: state.show_help(),
        steer_indicator: state.steer_indicator,
        scroll_offset: state.scroll_offset,
        scroll_locked: state.scroll_locked,
        chat_area_height: state.chat_area_height,
        pending_approval,
        pending_clarify: if let crate::app_state::TuiMode::ClarifyPending {
            question,
            context,
            choices,
            ..
        } = &state.mode
        {
            Some((question.clone(), choices.clone(), context.clone()))
        } else {
            None
        },
        pending_approval_input_snapshot: state.pending_approval_input_snapshot.clone(),
        input_history: state.input_history.clone(),
        history_index: state.history_index,
        draft: state.draft.clone(),
        input_area: state.layout.input_area.rect,
        chat_area: state.layout.chat_area.rect,
        clarify_banner: ratatui::layout::Rect::default(),
        chat_rendered_lines: state.chat_rendered_lines.clone(),
        chat_rendered_line_chrome_widths: state.chat_rendered_line_chrome_widths.clone(),
        chat_selection: state.chat_selection,
        chat_drag_active: state.chat_drag_active,
        clarify_banner_drag: match (
            state.clarify_choice_mousedown,
            state.clarify_banner_drag_end,
        ) {
            (Some((c1, r1)), Some((c2, r2))) => {
                let ((start_row, start_col), (end_row, end_col)) = if (r1, c1) <= (r2, c2) {
                    ((r1, c1), (r2, c2))
                } else {
                    ((r2, c2), (r1, c1))
                };
                Some(BannerDragSelection {
                    start_row,
                    start_col,
                    end_row,
                    end_col,
                })
            }
            _ => None,
        },
        toast: state.toast.clone(),
        spinner_frame: state.spinner_frame,
        help_scroll_offset: state.help_scroll_offset,
        help_scroll_x: state.help_scroll_x,
        clarify_scroll_offset: state.clarify_scroll_offset,
        input_scroll_top: state.input_scroll_top,
        bubble_title_rows: state.bubble_title_rows.clone(),
    }
}

/// Sync the view-geometry fields that [`crate::ui::draw`] writes back into
/// the canonical [`AppState`] after each frame.
fn sync_from_draw(state: &mut AppState, rendered: TuiState, button_areas: Option<ButtonAreas>) {
    state.layout.input_area.rect = rendered.input_area;
    state.layout.chat_area.rect = rendered.chat_area;
    state.layout.clarify_banner = rendered.clarify_banner;
    if let Some(buttons) = button_areas {
        state.layout.approve_button = buttons.approve;
        state.layout.deny_button = buttons.deny;
    } else {
        state.layout.approve_button = ratatui::layout::Rect::default();
        state.layout.deny_button = ratatui::layout::Rect::default();
    }
    state.input_scroll_top = rendered.input_scroll_top;
    state.scroll_offset = rendered.scroll_offset;
    state.scroll_locked = rendered.scroll_locked;
    state.bubble_title_rows = rendered.bubble_title_rows;
    state.chat_rendered_lines = rendered.chat_rendered_lines;
    state.chat_rendered_line_chrome_widths = rendered.chat_rendered_line_chrome_widths;
    state.help_scroll_offset = rendered.help_scroll_offset;
    state.help_scroll_x = rendered.help_scroll_x;
    state.clarify_scroll_offset = rendered.clarify_scroll_offset;
    state.toast = rendered.toast;
}
