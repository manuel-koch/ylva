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

//! Agents popup rendering: root and subagent runs with status and duration.

use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::agent::subagent_manager::{STATUS_QUEUED, STATUS_RUNNING};
use crate::saturating_cast::saturating_u16;
use crate::theme::{
    active_model_color, chat_scrollbar_color, error_color, help_popup_border_color,
    list_selection_background_color, list_selection_foreground_color, list_text_color,
    muted_text_color, overlay_selection_color,
};
use crate::tui_reducer_state::{AgentRow, AgentRowStatus, SubagentOutcome};
use crate::tui_render_state::TuiState;

use super::chat::render_chat_scrollbar;
use super::elapsed::format_elapsed;
use super::maximum_panel_content_height;

const MARKER_COLUMN_WIDTH: usize = 2;
const STATUS_DURATION_GAP: &str = "   ";
const AWAITING_INPUT_TEXT: &str = "waiting for user input";
const CURRENT_MARKER: &str = "▶ ";
const NO_DURATION: &str = "–";
const TOOL_COUNT_SUFFIX: &str = " tools";
const MINIMUM_LABEL_WIDTH: usize = 4;
/// Border rows of the panel, above and below the list.
const PANEL_BORDER_HEIGHT: u16 = 2;
/// Narrowest input box the panel still renders in; below this it is skipped,
/// like the slash overlay.
const MINIMUM_PANEL_WIDTH: u16 = 4;
/// Rows the terminal must leave above the input for the panel to render: the
/// border plus one content row.
const MINIMUM_PANEL_TOP: u16 = 3;

/// Shortens `text` to at most `max_width` columns, ending in `…` when cut.
fn elide(text: &str, max_width: usize) -> String {
    if Line::from(text).width() <= max_width {
        return text.to_string();
    }
    let mut elided = String::new();
    let mut width = 0;
    for character in text.chars() {
        let character_width = Span::raw(character.to_string()).width();
        if width + character_width + 1 > max_width {
            break;
        }
        elided.push(character);
        width += character_width;
    }
    elided.push('…');
    elided
}

fn result_text(row: &AgentRow) -> &'static str {
    if row.awaiting_input {
        return AWAITING_INPUT_TEXT;
    }
    match row.status {
        AgentRowStatus::Root => "",
        AgentRowStatus::Running => STATUS_RUNNING,
        AgentRowStatus::Queued => STATUS_QUEUED,
        AgentRowStatus::Finished => row.outcome.map_or("", SubagentOutcome::label),
    }
}

fn duration_text(row: &AgentRow) -> String {
    match row.status {
        AgentRowStatus::Root | AgentRowStatus::Queued => String::new(),
        _ => row
            .elapsed
            .map_or_else(|| NO_DURATION.to_string(), format_elapsed),
    }
}

fn tool_count_text(row: &AgentRow) -> String {
    format!("{}{}", row.tool_call_count, TOOL_COUNT_SUFFIX)
}

fn tree_prefix(row: &AgentRow) -> &'static str {
    match (row.depth, row.is_last_sibling) {
        (0, _) => "",
        (_, true) => "└─ ",
        (_, false) => "├─ ",
    }
}

/// One line per agent: marker, label, then the status, the tool-call count and
/// the duration flush right. The status takes only the width it needs; the
/// tool-call counts share one right-aligned column of `tool_count_width` and
/// the durations one of `duration_width`.
fn row_line(
    row: &AgentRow,
    inner_width: usize,
    tool_count_width: usize,
    duration_width: usize,
    selected: bool,
) -> Line<'static> {
    let status = result_text(row);
    let status_width = Line::from(status).width();
    let trailing_width = if status_width + tool_count_width + duration_width == 0 {
        0
    } else {
        // One column separates the label from the status.
        1 + status_width
            + STATUS_DURATION_GAP.len()
            + tool_count_width
            + STATUS_DURATION_GAP.len()
            + duration_width
    };
    let label_width = inner_width
        .saturating_sub(MARKER_COLUMN_WIDTH + trailing_width)
        .max(MINIMUM_LABEL_WIDTH);
    let tree_prefix = tree_prefix(row);
    let label = format!(
        "{tree_prefix}{}",
        elide(
            &row.label,
            label_width.saturating_sub(Line::from(tree_prefix).width())
        )
    );
    let label_padding = label_width.saturating_sub(Line::from(label.as_str()).width());
    let marker = if row.is_current { CURRENT_MARKER } else { "  " };

    let base_style = match row.status {
        AgentRowStatus::Queued => Style::default().fg(muted_text_color()),
        _ => Style::default().fg(list_text_color()),
    };
    let result_style = match (row.status, row.outcome) {
        _ if row.awaiting_input => Style::default()
            .fg(overlay_selection_color())
            .add_modifier(Modifier::BOLD),
        (AgentRowStatus::Running, _) => Style::default().fg(active_model_color()),
        (_, Some(SubagentOutcome::Error)) => Style::default().fg(error_color()),
        _ => base_style,
    };
    let (base_style, result_style) = if selected {
        let selection = Style::default()
            .fg(list_selection_foreground_color())
            .bg(list_selection_background_color());
        (selection, selection)
    } else {
        (base_style, result_style)
    };

    Line::from(vec![
        Span::styled(marker.to_string(), base_style.add_modifier(Modifier::BOLD)),
        Span::styled(format!("{label}{}", " ".repeat(label_padding)), base_style),
        Span::styled(
            if trailing_width == 0 {
                String::new()
            } else {
                format!(" {status}")
            },
            result_style,
        ),
        Span::styled(
            if trailing_width == 0 {
                String::new()
            } else {
                format!(
                    "{STATUS_DURATION_GAP}{:>tool_count_width$}",
                    tool_count_text(row),
                    tool_count_width = tool_count_width
                )
            },
            base_style,
        ),
        Span::styled(
            if trailing_width == 0 {
                String::new()
            } else {
                format!(
                    "{STATUS_DURATION_GAP}{:>width$}",
                    duration_text(row),
                    width = duration_width
                )
            },
            base_style,
        ),
    ])
}

/// Draw the agents popup as a floating panel whose bottom edge sits directly
/// above the input box, positioned and sized like the slash-command overlay:
/// same left edge and width as the input, a list that grows upward, and the
/// shared cap on the space above the input so a session with many subagents
/// cannot hide the conversation entirely.
pub(super) fn draw_agents_popup(frame: &mut Frame, state: &mut TuiState, input_area: Rect) {
    let row_count = state.agents_popup.rows.len();
    if input_area.width < MINIMUM_PANEL_WIDTH || input_area.y < MINIMUM_PANEL_TOP {
        state.agents_popup.popup_rect = Rect::default();
        state.agents_popup.row_hitboxes.clear();
        state.agents_popup.visible_rows = 0;
        return;
    }
    let content_height =
        saturating_u16(row_count).min(maximum_panel_content_height(input_area).max(1));
    let popup_height = content_height.saturating_add(PANEL_BORDER_HEIGHT);
    let popup = Rect::new(
        input_area.x,
        input_area.y.saturating_sub(popup_height),
        input_area.width,
        popup_height,
    );

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(help_popup_border_color()))
        .title(" Agents")
        .title_alignment(Alignment::Left);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let visible_rows = inner.height as usize;
    let selected_index = state.agents_popup.selected_index;
    let mut scroll_offset = state.agents_popup.scroll_offset;
    if state.agents_popup.reveal_selection {
        if selected_index < scroll_offset {
            scroll_offset = selected_index;
        } else if visible_rows > 0 && selected_index >= scroll_offset + visible_rows {
            scroll_offset = selected_index + 1 - visible_rows;
        }
    }
    scroll_offset = scroll_offset.min(row_count.saturating_sub(visible_rows));

    let overflows = row_count > visible_rows;
    let content_width = inner.width.saturating_sub(u16::from(overflows));

    let duration_width = state
        .agents_popup
        .rows
        .iter()
        .map(|row| Line::from(duration_text(row)).width())
        .max()
        .unwrap_or(0);
    let tool_count_width = state
        .agents_popup
        .rows
        .iter()
        .map(|row| Line::from(tool_count_text(row)).width())
        .max()
        .unwrap_or(0);
    let mut hitboxes = Vec::new();
    let mut lines = Vec::new();
    for (index, row) in state
        .agents_popup
        .rows
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(visible_rows)
    {
        lines.push(row_line(
            row,
            content_width as usize,
            tool_count_width,
            duration_width,
            index == selected_index,
        ));
        hitboxes.push((
            Rect::new(
                inner.x,
                inner.y + (index - scroll_offset) as u16,
                content_width,
                1,
            ),
            row.key,
        ));
    }
    frame.render_widget(Paragraph::new(lines), inner);
    if overflows {
        render_chat_scrollbar(
            frame,
            Rect::new(
                inner.x + content_width,
                inner.y,
                inner.width - content_width,
                inner.height,
            ),
            scroll_offset,
            row_count - visible_rows,
            row_count,
            visible_rows,
            chat_scrollbar_color(),
        );
    }

    state.agents_popup.popup_rect = popup;
    state.agents_popup.row_hitboxes = hitboxes;
    state.agents_popup.visible_rows = visible_rows;
    state.agents_popup.scroll_offset = scroll_offset;
    state.agents_popup.reveal_selection = false;
}

#[cfg(test)]
mod tests;
