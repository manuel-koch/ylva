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

use super::ClipboardSink;
use crate::events::AppAction;
use crate::presenter::TuiEvent;
use crate::tui_reducer_state::{
    AppState, Focus, HistoryDirection, InputEvent, SlashOverlay, LARGE_PASTE_CHAR_THRESHOLD,
};
use ratatui::style::Style;
use ratatui_textarea::TextArea;

/// Apply the standard Ylva textarea configuration: word-or-glyph wrapping and
/// no cursor-line underline decoration.
pub(super) fn configure_textarea(textarea: &mut TextArea<'_>) {
    textarea.set_wrap_mode(ratatui_textarea::WrapMode::WordOrGlyph);
    textarea.set_cursor_line_style(Style::default());
}

fn is_parent_view_key(event: &crossterm::event::KeyEvent) -> bool {
    use crossterm::event::{KeyCode, KeyModifiers};
    event.code == KeyCode::Char('p') && event.modifiers == KeyModifiers::CONTROL
}

/// Map an [`AppAction`] to the typed [`TuiEvent`] the Presenter accepts.
/// Returns `None` for actions that require no presenter call (e.g. `Resize`).
pub(super) fn app_action_to_tui_event(action: AppAction) -> Option<TuiEvent> {
    match action {
        AppAction::CtrlC => Some(TuiEvent::Input(InputEvent::CtrlC)),
        AppAction::Escape => Some(TuiEvent::Input(InputEvent::Escape)),
        AppAction::ShowHelp => Some(TuiEvent::Input(InputEvent::ShowHelp)),
        AppAction::StopSignal => Some(TuiEvent::Input(InputEvent::StopSignal)),
        AppAction::KeyInput(event) if is_parent_view_key(&event) => {
            Some(TuiEvent::Input(InputEvent::ParentView))
        }
        AppAction::KeyInput(event) => Some(TuiEvent::Input(InputEvent::KeyInput(event))),
        AppAction::Paste(text) => Some(TuiEvent::Input(InputEvent::Paste(text))),
        AppAction::MouseInput(mouse) => Some(TuiEvent::Mouse(mouse)),
        AppAction::Resize { .. } => None,
    }
}

/// Tracks a mouse press inside the input box so drag events can start a
/// text selection on first movement.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct MouseDrag {
    moved: bool,
}

impl MouseDrag {
    pub(super) fn place_cursor(
        &mut self,
        state: &mut AppState,
        textarea: &mut TextArea<'_>,
        column: u16,
        row: u16,
    ) {
        if move_input_cursor_to_mouse(
            textarea,
            state.layout.input_area.rect,
            state.input_scroll_top,
            column,
            row,
        ) {
            state.exit_input_history_browsing(&textarea.lines().join("\n"));
            textarea.cancel_selection();
            *self = Self::default();
        }
    }

    pub(super) fn start_selection(&mut self, textarea: &mut TextArea<'_>) {
        textarea.start_selection();
        self.moved = true;
    }

    pub(super) fn extend_selection(
        &mut self,
        state: &AppState,
        textarea: &mut TextArea<'_>,
        column: u16,
        row: u16,
    ) {
        let input_area = state.layout.input_area.rect;
        if input_area.contains(ratatui::layout::Position::new(column, row)) && column > input_area.x
        {
            if !self.moved {
                self.start_selection(textarea);
            }
            move_input_cursor_to_mouse(textarea, input_area, state.input_scroll_top, column, row);
        }
    }

    pub(super) fn finalize_selection(
        &mut self,
        state: &mut AppState,
        textarea: &mut TextArea<'_>,
        clipboard: &mut dyn ClipboardSink,
    ) {
        if self.moved {
            textarea.copy();
            let yanked = textarea.yank_text();
            if !yanked.is_empty() && clipboard.write_text(yanked) {
                state.set_toast("Copied to clipboard");
            }
        }
        *self = Self::default();
    }
}

pub(super) fn clear_textarea(textarea: &mut TextArea<'_>) {
    *textarea = TextArea::default();
    configure_textarea(textarea);
}

pub(super) fn set_input_text(textarea: &mut TextArea<'_>, text: &str) {
    let lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
    *textarea = TextArea::from(lines);
    configure_textarea(textarea);
    textarea.move_cursor(ratatui_textarea::CursorMove::End);
}

/// Hard upper bound on a single paste. Anything larger exceeds the context of
/// most LLMs anyway and is rejected to protect against memory blow-ups.
pub(super) const MAX_PASTE_BYTES: usize = 1024 * 1024;

pub(super) fn paste_text(state: &mut AppState, textarea: &mut TextArea<'_>, text: &str) {
    if text.len() > MAX_PASTE_BYTES {
        state.set_toast(format!(
            "Paste rejected: larger than {} MB",
            MAX_PASTE_BYTES / (1024 * 1024)
        ));
        return;
    }
    state.exit_input_history_browsing(&textarea.lines().join("\n"));
    let dedented_text = dedent(text);
    // One chunk insert refreshes the textarea's screen map once; per-key
    // input would re-wrap the whole draft for every character.
    match state
        .pasted_blobs
        .collapse(&dedented_text, LARGE_PASTE_CHAR_THRESHOLD)
    {
        Some(marker) => textarea.insert_str(marker),
        None => textarea.insert_str(dedented_text),
    };
}

pub(super) fn scroll_input_at_mouse(
    state: &AppState,
    textarea: &mut TextArea<'_>,
    mouse: crossterm::event::MouseEvent,
) -> bool {
    use crossterm::event::MouseEventKind;

    if matches!(
        mouse.kind,
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
    ) && state.layout.input_area.contains(mouse.column, mouse.row)
        && !matches!(state.focus, Focus::HelpPopup)
        && !matches!(state.slash_overlay, SlashOverlay::SessionPicker(_))
    {
        let delta = if matches!(mouse.kind, MouseEventKind::ScrollUp) {
            -1i16
        } else {
            1i16
        };
        textarea.scroll(ratatui_textarea::Scrolling::Delta {
            rows: delta,
            cols: 0,
        });
        true
    } else {
        false
    }
}

/// Dedent `text`: remove the common leading whitespace from all non-empty
/// lines (via `textwrap::dedent`), then strip leading/trailing blank-only lines.
pub(super) fn dedent(text: &str) -> String {
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

/// Place the cursor using the textarea's own wrapped screen map.
/// Returns `false` for points outside the content area.
fn move_input_cursor_to_mouse(
    textarea: &mut TextArea<'_>,
    area: ratatui::layout::Rect,
    scroll_top: usize,
    column: u16,
    row: u16,
) -> bool {
    use ratatui_textarea::CursorMove;

    if !area.contains(ratatui::layout::Position::new(column, row)) || column <= area.x {
        return false;
    }
    let target_row = scroll_top.saturating_add((row - area.y) as usize);
    let target_column = (column - area.x - 1) as usize;

    // The dependency exposes screen positions but not screen-to-data conversion.
    // Its native movements preserve wrapping, tabs, and Unicode display widths.
    textarea.move_cursor(CursorMove::Head);
    loop {
        let current_row = textarea.screen_cursor().row;
        if current_row == target_row {
            break;
        }
        textarea.move_cursor(if current_row < target_row {
            CursorMove::Down
        } else {
            CursorMove::Up
        });
        if textarea.screen_cursor().row == current_row {
            textarea.move_cursor(CursorMove::End);
            return true;
        }
    }
    loop {
        let previous_cursor = textarea.cursor();
        textarea.move_cursor(CursorMove::Forward);
        let screen_cursor = textarea.screen_cursor();
        if screen_cursor.row != target_row || screen_cursor.col > target_column {
            textarea.move_cursor(CursorMove::Back);
            break;
        }
        if textarea.cursor() == previous_cursor {
            break;
        }
    }
    true
}

/// Loads a history entry into the textarea. Returns `true` when a history
/// entry was loaded.
pub(super) fn load_history_into_textarea(
    direction: HistoryDirection,
    state: &mut AppState,
    textarea: &mut TextArea<'_>,
) -> bool {
    let current = textarea.lines().join("\n");
    if let Some(loaded) = state.navigate_input_message_history(direction, &current) {
        set_input_text(textarea, &loaded);
        true
    } else {
        false
    }
}
