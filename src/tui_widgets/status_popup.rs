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

//! Status popup rendering (`/status`): provider and engine, agent type, and
//! the context window's usage by origin as a stacked bar plus legend.

use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph};
use ratatui::Frame;

use crate::agent::{StatusSnapshot, StatusTimelineEntry};
use crate::context_breakdown::{format_percentage, ContextBreakdown, ContextGroup};
use crate::llm::token_estimate::format_token_estimate;
use crate::store::ModelParameters;
use crate::theme::{
    chat_scrollbar_color, chat_selection_bg, content_color, help_popup_border_color,
    help_popup_heading_color, muted_text_color, status_free_segment_color,
    status_segment_blue_color, status_segment_brown_color, status_segment_cyan_color,
    status_segment_green_color, status_segment_lime_color, status_segment_magenta_color,
    status_segment_mint_color, status_segment_orange_color, status_segment_pink_color,
    status_segment_red_color, status_segment_violet_color, status_segment_yellow_color,
};
use crate::tui_render_state::TuiState;

use super::chat::{apply_selection_highlight_to_line, render_chat_scrollbar};

const LABEL_WIDTH: usize = 16;
const BAR_FILLED: &str = "█";
const BAR_FREE: &str = "░";
const LEGEND_MARKER: &str = "■ ";
/// Below this content width the legend drops its "% used" column.
const WINDOW_COLUMN_MINIMUM_WIDTH: usize = 48;
const MINIMUM_POPUP_WIDTH: u16 = 30;
/// Columns left free on each side of the popup.
const POPUP_SIDE_MARGIN: u16 = 3;
const MINIMUM_POPUP_HEIGHT: u16 = 8;

fn segment_palette() -> [Color; 12] {
    [
        status_segment_blue_color(),
        status_segment_green_color(),
        status_segment_yellow_color(),
        status_segment_magenta_color(),
        status_segment_cyan_color(),
        status_segment_red_color(),
        status_segment_orange_color(),
        status_segment_lime_color(),
        status_segment_violet_color(),
        status_segment_pink_color(),
        status_segment_mint_color(),
        status_segment_brown_color(),
    ]
}

fn heading(text: &str) -> Line<'static> {
    Line::styled(
        text.to_string(),
        Style::default()
            .fg(help_popup_heading_color())
            .add_modifier(Modifier::BOLD),
    )
}

fn field(label: &str, value: impl Into<String>) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label:<LABEL_WIDTH$}"),
            Style::default().fg(muted_text_color()),
        ),
        Span::styled(value.into(), Style::default().fg(content_color())),
    ])
}

/// The allowed tools sorted by call count (most used first), then by name,
/// each with its call count in parentheses when it was used. Calls to tools
/// that are not allowed (invalid calls) are not listed.
fn tool_labels(snapshot: &StatusSnapshot) -> Vec<String> {
    let call_count = |name: &String| snapshot.tool_call_counts.get(name).copied().unwrap_or(0);
    let mut names: Vec<&String> = snapshot.allowed_tools.iter().collect();
    names.sort_by(|left, right| {
        call_count(right)
            .cmp(&call_count(left))
            .then_with(|| left.cmp(right))
    });
    names
        .into_iter()
        .map(|name| match snapshot.tool_call_counts.get(name) {
            Some(count) => format!("{name} ({count})"),
            None => name.clone(),
        })
        .collect()
}

/// Greedy word wrap of `items` joined by `, ` into lines of at most `width` columns.
fn wrap_comma_separated(items: &[String], width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let piece = if index + 1 < items.len() {
            format!("{item}, ")
        } else {
            item.clone()
        };
        match lines.last_mut() {
            Some(line) if line.chars().count() + piece.chars().count() <= width => {
                line.push_str(&piece)
            }
            _ => lines.push(piece),
        }
    }
    lines
        .into_iter()
        .map(|line| line.trim_end().to_string())
        .collect()
}

/// End column of each segment in a bar of `width` cells, cumulative so
/// rounding never makes the bar longer than the window.
fn segment_end_columns(
    breakdown: &ContextBreakdown,
    width: usize,
    denominator: usize,
) -> Vec<usize> {
    let window = denominator;
    let mut cumulative = 0;
    breakdown
        .segments()
        .iter()
        .map(|segment| {
            cumulative += segment.tokens;
            (cumulative * width + window / 2)
                .checked_div(window)
                .unwrap_or(0)
        })
        .collect()
}

/// Bar of the segments relative to the whole context window; the free
/// remainder is drawn as a dim tail.
fn usage_bar(breakdown: &ContextBreakdown, width: usize) -> Line<'static> {
    segment_bar(breakdown, width, breakdown.window_tokens())
}

/// Bar of the segments relative to the tokens actually used, so it is always
/// completely filled once anything is used.
fn used_bar(breakdown: &ContextBreakdown, width: usize) -> Line<'static> {
    segment_bar(breakdown, width, breakdown.used_tokens())
}

fn segment_bar(breakdown: &ContextBreakdown, width: usize, denominator: usize) -> Line<'static> {
    let palette = segment_palette();
    let mut spans = Vec::new();
    let mut filled = 0;
    for (index, end) in segment_end_columns(breakdown, width, denominator)
        .into_iter()
        .enumerate()
    {
        let end = end.min(width);
        if end > filled {
            spans.push(Span::styled(
                BAR_FILLED.repeat(end - filled),
                Style::default().fg(palette[index % palette.len()]),
            ));
            filled = end;
        }
    }
    if filled < width {
        spans.push(Span::styled(
            BAR_FREE.repeat(width - filled),
            Style::default().fg(status_free_segment_color()),
        ));
    }
    Line::from(spans)
}

/// Right-aligned number columns of the contribution table.
fn amount_columns(window: &str, used: &str, tokens: &str, show_window_column: bool) -> String {
    if show_window_column {
        format!("{window:>8}  {used:>8}  {tokens:>7}")
    } else {
        format!("{used:>7}  {tokens:>7}")
    }
}

fn table_row(
    marker: Span<'static>,
    label: String,
    amount: String,
    amount_style: Style,
    label_style: Style,
    width: usize,
) -> Line<'static> {
    let label_width = width.saturating_sub(LEGEND_MARKER.chars().count() + amount.len() + 1);
    let label: String = label.chars().take(label_width.max(1)).collect();
    let padding = label_width.saturating_sub(label.chars().count());
    Line::from(vec![
        marker,
        Span::styled(label, label_style),
        Span::raw(" ".repeat(padding + 1)),
        Span::styled(amount, amount_style),
    ])
}

fn table_header(width: usize) -> Line<'static> {
    let style = Style::default()
        .fg(help_popup_heading_color())
        .add_modifier(Modifier::BOLD);
    table_row(
        Span::raw(" ".repeat(LEGEND_MARKER.chars().count())),
        "Contribution".to_string(),
        amount_columns(
            "% window",
            "% used",
            "Tokens",
            width >= WINDOW_COLUMN_MINIMUM_WIDTH,
        ),
        style,
        style,
        width,
    )
}

/// One contribution row; the free remainder is not part of the used context,
/// so it shows `-` instead of a used percentage.
fn legend_line(
    marker_color: Color,
    label: String,
    tokens: usize,
    breakdown: &ContextBreakdown,
    include_used_percentage: bool,
    width: usize,
) -> Line<'static> {
    let window_percentage = format_percentage(breakdown.percentage(tokens));
    let used_percentage = if include_used_percentage {
        format_percentage(breakdown.percentage_of_used(tokens))
    } else {
        "-".to_string()
    };
    table_row(
        Span::styled(LEGEND_MARKER, Style::default().fg(marker_color)),
        label,
        amount_columns(
            &window_percentage,
            &used_percentage,
            &format_token_estimate(tokens),
            width >= WINDOW_COLUMN_MINIMUM_WIDTH,
        ),
        Style::default().fg(muted_text_color()),
        Style::default().fg(content_color()),
        width,
    )
}

/// Every content line of the popup for a content area `width` columns wide.
pub(super) fn status_lines(snapshot: &StatusSnapshot, width: usize) -> Vec<Line<'static>> {
    let breakdown = &snapshot.breakdown;
    let mut lines = vec![
        heading("Current setup"),
        field(
            "Agent type",
            snapshot.agent_type.as_deref().unwrap_or("default"),
        ),
    ];
    if !snapshot.engine_name.is_empty() {
        lines.push(field("Engine", snapshot.engine_name.clone()));
    }
    lines.push(field("Provider", snapshot.provider_connection.clone()));
    lines.push(field("Model", snapshot.model.clone()));
    if let Some(reasoning_effort) = &snapshot.reasoning_effort {
        lines.push(field("Reasoning", reasoning_effort.clone()));
    }
    if let Some(temperature) = &snapshot.temperature {
        lines.push(field("Temperature", temperature.clone()));
    }
    if let Some(max_output_tokens) = snapshot.max_output_tokens {
        lines.push(field(
            "Max output",
            format!("{} tokens", format_token_estimate(max_output_tokens)),
        ));
    }
    lines.push(field(
        "Context window",
        format!(
            "{} tokens",
            format_token_estimate(breakdown.context_length())
        ),
    ));
    let tool_labels = tool_labels(snapshot);
    let tool_names = wrap_comma_separated(&tool_labels, width.saturating_sub(LABEL_WIDTH).max(1));
    for (index, tool_line) in tool_names.into_iter().enumerate() {
        lines.push(field(if index == 0 { "Tools" } else { "" }, tool_line));
    }
    if tool_labels.is_empty() {
        lines.push(field("Tools", "none"));
    }

    append_timeline(&mut lines, snapshot, width);

    lines.push(Line::default());
    lines.push(heading(&format!(
        "Context usage (estimated, {} of window, {} free)",
        format_percentage(breakdown.percentage(breakdown.used_tokens())),
        format_percentage(breakdown.percentage(breakdown.free_tokens()))
    )));
    lines.push(Line::styled(
        "Relative to avail context size",
        Style::default().fg(muted_text_color()),
    ));
    lines.push(usage_bar(breakdown, width));
    lines.push(Line::styled(
        "Relative to used context size",
        Style::default().fg(muted_text_color()),
    ));
    lines.push(used_bar(breakdown, width));
    lines.push(Line::default());
    lines.push(table_header(width));
    let palette = segment_palette();
    for group in [
        ContextGroup::SystemPrompt,
        ContextGroup::CompactionSummary,
        ContextGroup::UserMessages,
        ContextGroup::AssistantMessages,
        ContextGroup::ToolCalls,
    ] {
        let members: Vec<_> = breakdown
            .segments()
            .iter()
            .enumerate()
            .filter(|(_, segment)| segment.category.group() == group)
            .collect();
        if members.is_empty() {
            continue;
        }
        if members.len() > 1 {
            lines.push(Line::styled(
                group.label().to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ));
        }
        for (index, segment) in members {
            lines.push(legend_line(
                palette[index % palette.len()],
                segment.category.label(),
                segment.tokens,
                breakdown,
                true,
                width,
            ));
        }
    }
    lines.push(legend_line(
        status_free_segment_color(),
        "Free".to_string(),
        breakdown.free_tokens(),
        breakdown,
        false,
        width,
    ));
    if snapshot.finished_subagent {
        lines.push(Line::default());
        for note_line in wrap_hanging(
            "Finished subagent: rebuilt from its transcript. System prompt and \
             tool schemas were not recorded.",
            width,
            0,
        ) {
            lines.push(Line::styled(
                note_line,
                Style::default().fg(muted_text_color()),
            ));
        }
    }
    lines
}

fn line_text(line: &Line) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

/// Columns of the timestamp label plus its two-space separator; wrapped
/// history lines continue under the event text.
const HISTORY_TEXT_INDENT: usize = 19;

/// Greedy word wrap of `text` into lines of at most `width` columns; every
/// continuation line is indented by `indent` columns. Words longer than a line
/// are split.
fn wrap_hanging(text: &str, width: usize, indent: usize) -> Vec<String> {
    let width = width.max(1);
    let indent = if indent + 8 > width { 0 } else { indent };
    let capacity = |line_index: usize| {
        if line_index == 0 {
            width
        } else {
            width - indent
        }
    };
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_length = 0;
    for word in text.split(' ') {
        let characters: Vec<char> = word.chars().collect();
        let mut remaining = characters.as_slice();
        loop {
            let separator = usize::from(current_length > 0);
            let limit = capacity(lines.len());
            if current_length + separator + remaining.len() <= limit {
                if separator == 1 {
                    current.push(' ');
                }
                current.extend(remaining);
                current_length += separator + remaining.len();
                break;
            }
            if current_length > 0 {
                lines.push(std::mem::take(&mut current));
                current_length = 0;
                continue;
            }
            let (head, tail) = remaining.split_at(limit);
            lines.push(head.iter().collect());
            remaining = tail;
        }
    }
    lines.push(current);
    lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            if index == 0 {
                line
            } else {
                format!("{}{line}", " ".repeat(indent))
            }
        })
        .collect()
}

/// `YYYY-MM-DD HH:MMZ` of an RFC 3339 UTC timestamp, so sessions spanning
/// several days stay readable; blank padding when unknown.
fn timestamp_label(timestamp: &str) -> String {
    match (timestamp.get(0..10), timestamp.get(11..16)) {
        (Some(date), Some(clock)) => format!("{date} {clock}Z"),
        _ => " ".repeat(17),
    }
}

/// The model and the parameters that differ from `previous` (all set ones for
/// the first model entry).
fn describe_model(
    provider_connection: &str,
    model: &str,
    parameters: &ModelParameters,
    previous: Option<&ModelParameters>,
) -> String {
    let default_parameters = ModelParameters::default();
    let previous = previous.unwrap_or(&default_parameters);
    let mut parts = vec![format!("{provider_connection}/{model}")];
    if parameters.engine_name != previous.engine_name {
        parts.extend(parameters.engine_name.clone());
    }
    if let Some(context_length) = parameters.context_length {
        if parameters.context_length != previous.context_length {
            parts.push(format!("{} window", format_token_estimate(context_length)));
        }
    }
    if parameters.reasoning_effort != previous.reasoning_effort {
        parts.extend(
            parameters
                .reasoning_effort
                .clone()
                .map(|effort| format!("effort {effort}")),
        );
    }
    if parameters.temperature != previous.temperature {
        parts.extend(
            parameters
                .temperature
                .clone()
                .map(|temperature| format!("temp {temperature}")),
        );
    }
    if let Some(max_output_tokens) = parameters.max_output_tokens {
        if parameters.max_output_tokens != previous.max_output_tokens {
            parts.push(format!(
                "max out {}",
                format_token_estimate(max_output_tokens)
            ));
        }
    }
    parts.join(" · ")
}

/// Model changes and compactions in order, one text line each; compactions show
/// the estimated tokens before and after and the change.
fn append_timeline(lines: &mut Vec<Line<'static>>, snapshot: &StatusSnapshot, width: usize) {
    lines.push(Line::default());
    lines.push(heading("History"));
    let mut previous_parameters: Option<&ModelParameters> = None;
    let mut compaction_number = 0;
    let mut reductions = Vec::new();
    for entry in &snapshot.timeline {
        let (text, style) = match entry {
            StatusTimelineEntry::Model {
                at,
                provider_connection,
                model,
                parameters,
            } => {
                let text = format!(
                    "{}  model changed: {}",
                    timestamp_label(at),
                    describe_model(provider_connection, model, parameters, previous_parameters)
                );
                previous_parameters = Some(parameters);
                (text, Style::default().fg(content_color()))
            }
            StatusTimelineEntry::Compaction { at, effect } => {
                compaction_number += 1;
                let outcome = match effect {
                    Some(effect) => {
                        reductions.push(effect.reduction_percent());
                        format!(
                            "{} → {} tokens ({})",
                            format_token_estimate(effect.estimated_tokens_before),
                            format_token_estimate(effect.estimated_tokens_after),
                            format_percentage(-effect.reduction_percent())
                        )
                    }
                    None => "effect not recorded".to_string(),
                };
                (
                    format!(
                        "{}  compaction #{compaction_number}  {outcome}",
                        timestamp_label(at)
                    ),
                    Style::default().fg(content_color()),
                )
            }
        };
        for wrapped in wrap_hanging(&text, width, HISTORY_TEXT_INDENT) {
            lines.push(Line::styled(wrapped, style));
        }
    }
    if reductions.len() > 1 {
        let average = reductions.iter().sum::<f64>() / reductions.len() as f64;
        lines.push(field("Average reduction", format_percentage(average)));
    }
}

pub(super) fn draw_status_popup(frame: &mut Frame, state: &mut TuiState) {
    let Some(snapshot) = state.status_popup.snapshot.as_ref() else {
        return;
    };
    let area = frame.area();
    if area.width < MINIMUM_POPUP_WIDTH || area.height < MINIMUM_POPUP_HEIGHT {
        state.status_popup.popup_rect = Rect::default();
        state.status_popup.content_area = Rect::default();
        state.status_popup.visible_rows = 0;
        return;
    }
    let popup_width = area.width.saturating_sub(2 * POPUP_SIDE_MARGIN);
    let x = area.x + area.width.saturating_sub(popup_width) / 2;
    let chat_area = state.conversation.chat_area;
    // Use the whole conversation list height once the chat area is known.
    let popup = if chat_area.height >= MINIMUM_POPUP_HEIGHT {
        Rect::new(x, chat_area.y, popup_width, chat_area.height)
    } else {
        let popup_height = (area.height * 80 / 100).max(MINIMUM_POPUP_HEIGHT);
        Rect::new(
            x,
            area.y + area.height.saturating_sub(popup_height) / 2,
            popup_width,
            popup_height,
        )
    };
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(help_popup_border_color()))
        .title(" Status")
        .title_alignment(Alignment::Left)
        .padding(Padding::uniform(1));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let visible_rows = inner.height as usize;
    // Reserve a one-column gap plus the scrollbar column only when the content
    // overflows; wrapping depends on the width, so measure the full width first.
    let full_lines = status_lines(snapshot, inner.width as usize);
    let overflows = full_lines.len() > visible_rows;
    let content_width = inner.width.saturating_sub(2 * u16::from(overflows));
    let lines = if overflows {
        status_lines(snapshot, content_width as usize)
    } else {
        full_lines
    };
    let line_count = lines.len();
    let mut lines = lines;
    let rendered_lines: Vec<String> = lines.iter().map(line_text).collect();
    if let Some(selection) = state.status_popup.selection.filter(|s| !s.is_empty()) {
        let (start_line, start_column, end_line, end_column) = selection.normalized();
        for (line_index, line) in lines.iter_mut().enumerate() {
            if line_index < start_line || line_index > end_line {
                continue;
            }
            let column_start = if line_index == start_line {
                start_column
            } else {
                0
            };
            let column_end = if line_index == end_line {
                end_column
            } else {
                usize::MAX
            };
            apply_selection_highlight_to_line(line, column_start, column_end, chat_selection_bg());
        }
    }
    let scroll_offset = state
        .status_popup
        .scroll_offset
        .min(line_count.saturating_sub(visible_rows));
    let visible: Vec<Line> = lines
        .into_iter()
        .skip(scroll_offset)
        .take(visible_rows)
        .collect();
    frame.render_widget(
        Paragraph::new(visible),
        Rect::new(inner.x, inner.y, content_width, inner.height),
    );
    if overflows {
        render_chat_scrollbar(
            frame,
            Rect::new(inner.x + content_width + 1, inner.y, 1, inner.height),
            scroll_offset,
            line_count - visible_rows,
            line_count,
            visible_rows,
            chat_scrollbar_color(),
        );
    }

    state.status_popup.content_area = Rect::new(inner.x, inner.y, content_width, inner.height);
    state.status_popup.rendered_lines = rendered_lines;
    state.status_popup.popup_rect = popup;
    state.status_popup.visible_rows = visible_rows;
    state.status_popup.scroll_offset = scroll_offset;
}

#[cfg(test)]
mod tests;
