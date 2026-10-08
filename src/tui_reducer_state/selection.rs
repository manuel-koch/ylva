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

//! Selection state and operations.

use super::AppState;

// ─── ChatSelection ───────────────────────────────────────────────────────────

/// Screen-space selection state for the chat area. Line and column are
/// Normalised (start ≤ end) drag selection inside the clarify banner, in
/// absolute screen coordinates. Used only for rendering the highlight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BannerDragSelection {
    pub start_row: u16,
    pub start_col: u16,
    pub end_row: u16,
    pub end_col: u16,
}

/// absolute line indices into `TuiState::chat_rendered_lines`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatSelection {
    pub anchor_line: usize,
    pub anchor_col: usize,
    pub cursor_line: usize,
    pub cursor_col: usize,
}

impl ChatSelection {
    /// Returns `(start_line, start_col, end_line, end_col)` in reading order.
    pub fn normalized(&self) -> (usize, usize, usize, usize) {
        if (self.anchor_line, self.anchor_col) <= (self.cursor_line, self.cursor_col) {
            (
                self.anchor_line,
                self.anchor_col,
                self.cursor_line,
                self.cursor_col,
            )
        } else {
            (
                self.cursor_line,
                self.cursor_col,
                self.anchor_line,
                self.anchor_col,
            )
        }
    }

    pub fn is_empty(&self) -> bool {
        self.anchor_line == self.cursor_line && self.anchor_col == self.cursor_col
    }
}

/// Keep complete graphemes intersecting the selected terminal columns.
fn display_column_range(text: &str, start: usize, end: usize) -> String {
    let span = ratatui::text::Span::raw(text);
    let mut column = 0;
    let mut selected = String::new();
    for grapheme in span.styled_graphemes(ratatui::style::Style::default()) {
        let width = ratatui::text::Span::raw(grapheme.symbol).width();
        if column < end && column + width > start {
            selected.push_str(grapheme.symbol);
        }
        column += width;
    }
    selected
}

fn extract_selection_text(
    selection: Option<&ChatSelection>,
    lines: &[String],
    prefix_widths: &[usize],
) -> Option<String> {
    let selection = selection?;
    if selection.is_empty() || lines.is_empty() {
        return None;
    }
    let (start_line, start_col, end_line, end_col) = selection.normalized();
    let start_line = start_line.min(lines.len().saturating_sub(1));
    let end_line = end_line.min(lines.len().saturating_sub(1));
    if start_line > end_line {
        return None;
    }
    let prefix_width = |index: usize| -> usize { prefix_widths.get(index).copied().unwrap_or(0) };
    if start_line == end_line {
        let line = &lines[start_line];
        let prefix = prefix_width(start_line);
        Some(display_column_range(
            line,
            start_col.saturating_sub(prefix),
            end_col.saturating_sub(prefix),
        ))
    } else {
        let mut result = String::new();
        for (absolute_index, line) in lines
            .iter()
            .enumerate()
            .skip(start_line)
            .take(end_line - start_line + 1)
        {
            let prefix = prefix_width(absolute_index);
            let fragment = if absolute_index == start_line {
                display_column_range(line, start_col.saturating_sub(prefix), usize::MAX)
            } else if absolute_index == end_line {
                display_column_range(line, 0, end_col.saturating_sub(prefix))
            } else {
                line.clone()
            };
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(&fragment);
        }
        Some(result)
    }
}

impl AppState {
    pub fn help_popup_content_position(&self, column: u16, row: u16) -> Option<(usize, usize)> {
        let content_area = self.layout.help_popup.content_area;
        if !content_area.contains((column, row).into()) {
            return None;
        }
        Some((
            self.help_popup.help_scroll_offset + row.saturating_sub(content_area.y) as usize,
            self.help_popup.help_scroll_x + column.saturating_sub(content_area.x) as usize,
        ))
    }

    pub fn help_popup_drag_position(&self, column: u16, row: u16) -> Option<(usize, usize)> {
        let content_area = self.layout.help_popup.content_area;
        if content_area.width == 0 || content_area.height == 0 {
            return None;
        }
        let max_column = content_area
            .x
            .saturating_add(content_area.width.saturating_sub(1));
        let max_row = content_area
            .y
            .saturating_add(content_area.height.saturating_sub(1));
        let clamped_column = column.clamp(content_area.x, max_column);
        let clamped_row = row.clamp(content_area.y, max_row);
        Some((
            self.help_popup.help_scroll_offset
                + clamped_row.saturating_sub(content_area.y) as usize,
            self.help_popup.help_scroll_x + clamped_column.saturating_sub(content_area.x) as usize,
        ))
    }

    /// Return the absolute rendered-line and column position under a header
    /// bubble cell.
    pub fn header_bubble_content_position(&self, column: u16, row: u16) -> Option<(usize, usize)> {
        let area = self.layout.header_bubble;
        if !area.contains((column, row).into()) {
            return None;
        }
        Some((
            self.child_view.header_bubble_scroll_offset + row.saturating_sub(area.y) as usize,
            column.saturating_sub(area.x) as usize,
        ))
    }

    /// Clamp a drag endpoint to the visible header bubble content area and
    /// return its absolute rendered-line and column position.
    pub fn header_bubble_drag_position(&self, column: u16, row: u16) -> Option<(usize, usize)> {
        let area = self.layout.header_bubble;
        if area.is_empty() {
            return None;
        }
        let max_column = area.x.saturating_add(area.width.saturating_sub(1));
        let max_row = area.y.saturating_add(area.height.saturating_sub(1));
        let clamped_column = column.clamp(area.x, max_column);
        let clamped_row = row.clamp(area.y, max_row);
        Some((
            self.child_view.header_bubble_scroll_offset
                + clamped_row.saturating_sub(area.y) as usize,
            clamped_column.saturating_sub(area.x) as usize,
        ))
    }

    /// Extract the text covered by the current `chat_selection`.
    pub fn extract_chat_selection_text(&self) -> Option<String> {
        extract_selection_text(
            self.conversation.chat_selection.as_ref(),
            &self.conversation.chat_rendered_lines,
            &self.conversation.chat_rendered_line_prefix_widths,
        )
    }

    /// Extract the text covered by the current Help-popup selection.
    pub fn extract_help_selection_text(&self) -> Option<String> {
        extract_selection_text(
            self.help_popup.help_selection.as_ref(),
            &self.help_popup.help_rendered_lines,
            &[],
        )
    }

    /// Extract the text covered by the current child-view header selection.
    pub fn extract_header_selection_text(&self) -> Option<String> {
        extract_selection_text(
            self.child_view.header_selection.as_ref(),
            &self.child_view.header_rendered_lines,
            &self.child_view.header_rendered_line_prefix_widths,
        )
    }
}
