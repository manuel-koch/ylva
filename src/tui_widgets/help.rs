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

//! Help content, popup layout and scroll/selection rendering.

use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph};
use ratatui::Frame;

use crate::build_info::BUILD_INFO;
use crate::home_relative_path::display_path;
use crate::theme::{chat_selection_bg, help_popup_border_color, help_popup_heading_color};
use crate::tui_reducer_state::{CONFIRM_TIMEOUT, HELP_HORIZONTAL_SCROLL_COLUMNS};
use crate::tui_render_state::TuiState;

use super::chat::apply_selection_highlight_to_line;

struct HelpSection {
    heading: &'static str,
    entries: Vec<(String, String)>,
}

fn help_entry(key: impl Into<String>, description: impl Into<String>) -> (String, String) {
    (key.into(), description.into())
}

fn help_sections() -> Vec<HelpSection> {
    let confirm_timeout_secs = CONFIRM_TIMEOUT.as_secs();
    vec![
        HelpSection {
            heading: "General",
            entries: vec![
                help_entry("F1", "Toggle this help"),
                help_entry("ctrl-c (x1)", "Show \"Press again to quit\" hint (idle)"),
                help_entry(
                    format!("ctrl-c (x2, <={confirm_timeout_secs}s)"),
                    "Quit TUI",
                ),
                help_entry(
                    "esc (x1)",
                    "Show \"Press again to cancel\" hint (busy, empty input)",
                ),
                help_entry(
                    format!("esc (x2, <={confirm_timeout_secs}s)"),
                    "Interrupt generation, TUI stays open",
                ),
                help_entry(
                    "ctrl-c (x1, busy)",
                    "Show \"Press again to cancel & quit\" hint (empty input)",
                ),
                help_entry(
                    format!("ctrl-c (x2, <={confirm_timeout_secs}s, busy)"),
                    "Interrupt generation, then quit",
                ),
                help_entry("esc", "Close popup"),
            ],
        },
        HelpSection {
            heading: "Draft message",
            entries: vec![
                help_entry("enter", "Send message; queue steer if busy"),
                help_entry("shift-enter / alt-enter", "Insert newline"),
                help_entry("ctrl-c / esc", "Clear input field (busy, non-empty input)"),
                help_entry("esc", "Clear input (idle)"),
                help_entry("↑ (on first line)", "Load previous message from history"),
                help_entry("↓ (on last line)", "Load next message from history"),
                help_entry(
                    "home / fn-left",
                    "Line start; at col 0: end of previous line",
                ),
                help_entry(
                    "end / fn-right",
                    "Line end; at line end: start of next line",
                ),
            ],
        },
        HelpSection {
            heading: "Help popup",
            entries: vec![
                help_entry("↑ / ↓", "Scroll up / down one line"),
                help_entry(
                    "← / →",
                    format!("Scroll left / right by {HELP_HORIZONTAL_SCROLL_COLUMNS} columns"),
                ),
                help_entry("page-up / page-down", "Scroll up / down"),
                help_entry("mouse wheel", "Scroll up / down"),
            ],
        },
        HelpSection {
            heading: "Environment",
            entries: vec![],
        },
        HelpSection {
            heading: "Slash commands",
            entries: crate::slash_command::slash_command_suggestions("/")
                .into_iter()
                .map(|command| help_entry(command.name, command.description))
                .collect(),
        },
        HelpSection {
            heading: "Session picker",
            entries: vec![
                help_entry("↑ / ↓, page keys, wheel", "Select a root session"),
                help_entry("← / →, action click", "Choose Resume / Clone"),
                help_entry(
                    "enter",
                    "Confirm when root/subagent/compaction work is idle",
                ),
                help_entry("esc", "Cancel; retain and report any created clone"),
            ],
        },
        HelpSection {
            heading: "Build",
            entries: vec![],
        },
    ]
}

pub(super) fn draw_help_popup(frame: &mut Frame, state: &mut TuiState) {
    let area = frame.area();
    let popup_width = (area.width * 75 / 100).max(20);
    let popup_height = (area.height * 70 / 100).max(10);
    let x = area.x + (area.width.saturating_sub(popup_width)) / 2;
    let y = area.y + (area.height.saturating_sub(popup_height)) / 2;
    let popup = Rect::new(x, y, popup_width, popup_height);
    // Store the popup rect so mouse-click handling can tell inside from
    // outside clicks (dismiss-on-outside-click; clicks inside must not close).
    state.help_popup.help_popup_rect = popup;

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(help_popup_border_color()))
        .title(" Help")
        .title_alignment(Alignment::Left)
        .padding(Padding::uniform(1));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    state.help_popup.help_content_area = inner;

    // Build content lines grouped into sections. Each section starts with a
    // heading line followed by its entries; sections are separated by a blank.
    // No leading space on heading text — the popup padding provides the margin.
    let heading_style = Style::default()
        .fg(help_popup_heading_color())
        .add_modifier(Modifier::BOLD);

    let sections = help_sections();

    let mut all_lines: Vec<Line> = Vec::new();
    for section in &sections {
        if !all_lines.is_empty() {
            all_lines.push(Line::default());
        }
        all_lines.push(Line::styled(section.heading, heading_style));
        match section.heading {
            "Environment" => {
                let environment_entries = [
                    (
                        "Ylva home:",
                        state
                            .display_metadata
                            .ylva_home
                            .as_deref()
                            .map_or_else(|| "not resolved".to_string(), display_path),
                    ),
                    ("Profile:", state.display_metadata.profile_name.clone()),
                    (
                        "Profile path:",
                        state
                            .display_metadata
                            .profile_path
                            .as_deref()
                            .map_or_else(|| "built-in default".to_string(), display_path),
                    ),
                    (
                        "Workspace root:",
                        state
                            .display_metadata
                            .workspace_root
                            .as_deref()
                            .map_or_else(|| "not discovered".to_string(), display_path),
                    ),
                ];
                let label_width = environment_entries
                    .iter()
                    .map(|(label, _)| label.chars().count())
                    .max()
                    .unwrap_or(0);
                for (label, value) in environment_entries {
                    all_lines.push(Line::from(format!("  {label:<label_width$} {value}")));
                }
            }
            "Build" => {
                let build_entries = BUILD_INFO
                    .render()
                    .lines()
                    .skip(1)
                    .filter_map(|line| line.split_once(':'))
                    .map(|(label, value)| (format!("{label}:"), value.trim_start().to_string()))
                    .collect::<Vec<_>>();
                let label_width = build_entries
                    .iter()
                    .map(|(label, _)| label.chars().count())
                    .max()
                    .unwrap_or(0);
                for (label, value) in build_entries {
                    all_lines.push(Line::from(format!("  {label:<label_width$} {value}")));
                }
            }
            _ => {
                // Align descriptions to the longest key within this section only,
                // so short-key sections don't inherit padding from long-key ones.
                let key_width = section
                    .entries
                    .iter()
                    .map(|(key, _)| key.chars().count())
                    .max()
                    .unwrap_or(0);
                for (key, desc) in &section.entries {
                    all_lines.push(Line::from(format!("  {key:<key_width$}  {desc}")));
                }
            }
        }
    }

    let total_lines = all_lines.len();
    let max_line_width = all_lines.iter().map(|l| l.width()).max().unwrap_or(0);
    state.help_popup.help_rendered_lines = all_lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect();

    let viewport_height = inner.height as usize;
    let viewport_width = inner.width as usize;

    // Clamp offsets so we never display empty space.
    let max_scroll_y = total_lines.saturating_sub(viewport_height);
    let max_scroll_x = max_line_width.saturating_sub(viewport_width);
    state.help_popup.help_scroll_offset = state.help_popup.help_scroll_offset.min(max_scroll_y);
    state.help_popup.help_scroll_x = state.help_popup.help_scroll_x.min(max_scroll_x);
    let scroll_y = state.help_popup.help_scroll_offset;
    let scroll_x = state.help_popup.help_scroll_x;

    if let Some(selection) = state.help_popup.help_selection {
        if !selection.is_empty() {
            let (start_line, start_col, end_line, end_col) = selection.normalized();
            for (line_index, line) in all_lines.iter_mut().enumerate() {
                if line_index < start_line || line_index > end_line {
                    continue;
                }
                let column_start = if line_index == start_line {
                    start_col
                } else {
                    0
                };
                let column_end = if line_index == end_line {
                    end_col
                } else {
                    usize::MAX
                };
                apply_selection_highlight_to_line(
                    line,
                    column_start,
                    column_end,
                    chat_selection_bg(),
                );
            }
        }
    }

    frame.render_widget(
        Paragraph::new(all_lines).scroll((scroll_y as u16, scroll_x as u16)),
        inner,
    );

    // Scroll indicators sit in the 1-char padding strip between the border and
    // the content, so they never obscure text.
    //
    // Right padding column  = popup.x + popup.width  - 2
    // Left  padding column  = popup.x + 1
    // Top   padding row     = popup.y + 1
    // Bottom padding row    = popup.y + popup.height - 2
    let can_scroll_up = scroll_y > 0;
    let can_scroll_down = scroll_y < max_scroll_y;
    let can_scroll_left = scroll_x > 0;
    let can_scroll_right = scroll_x < max_scroll_x;
    let indicator_style = Style::default().fg(help_popup_border_color());

    let pad_col_right = popup.x + popup.width.saturating_sub(2);
    let pad_col_left = popup.x + 1;
    let pad_row_top = popup.y + 1;
    let pad_row_bottom = popup.y + popup.height.saturating_sub(2);
    let pad_row_mid = popup.y + popup.height / 2;

    if can_scroll_up || can_scroll_right {
        let symbol = match (can_scroll_up, can_scroll_right) {
            (true, true) => "↗",
            (true, false) => "↑",
            (false, true) => "→",
            (false, false) => unreachable!(),
        };
        frame.render_widget(
            Paragraph::new(symbol).style(indicator_style),
            Rect::new(pad_col_right, pad_row_top, 1, 1),
        );
    }
    if can_scroll_down {
        let symbol = if can_scroll_right { "↘" } else { "↓" };
        frame.render_widget(
            Paragraph::new(symbol).style(indicator_style),
            Rect::new(pad_col_right, pad_row_bottom, 1, 1),
        );
    }
    if can_scroll_left {
        frame.render_widget(
            Paragraph::new("←").style(indicator_style),
            Rect::new(pad_col_left, pad_row_mid, 1, 1),
        );
    }
}

#[cfg(test)]
mod tests;
