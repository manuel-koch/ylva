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

//! Slash/model panels, clarify banners and transient notifications.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::markdown::render_markdown;
use crate::saturating_cast::saturating_u16;
use crate::theme::{
    ACTIVE_MODEL_COLOR, CHAT_SCROLLBAR_COLOR, CLARIFY_BANNER_FG_COLOR,
    CLARIFY_BANNER_SCROLL_INDICATOR_COLOR, CLARIFY_BANNER_SELECTION_FG_COLOR,
    COMPACTION_MISMATCH_COLOR, LIST_BORDER_COLOR, LIST_SELECTION_BACKGROUND_COLOR,
    LIST_SELECTION_FOREGROUND_COLOR, LIST_TEXT_COLOR, MUTED_TEXT_COLOR, OVERLAY_SELECTION_COLOR,
    TOAST_BG_COLOR, TOAST_FG_COLOR,
};
use crate::tui_reducer_state::{
    matching_model_indices, model_picker_row_entries, model_picker_row_is_details,
    model_picker_row_layout, slash_overlay_text_rows, visible_model_choices, BannerDragSelection,
    PickerRowLayout, SlashOverlay, SlashOverlayLayout,
};

use super::chat::{apply_selection_highlight_to_line, render_chat_scrollbar};
use super::footer::elide_balanced_tokens;

const SESSION_RESUME_BUTTON_LABEL: &str = " Resume ";
const SESSION_CLONE_BUTTON_LABEL: &str = " Clone ";
const SESSION_ACTION_BUTTON_GAP: &str = "  ";
/// Indent of a `/models` details row, aligning it under the identity text
/// (past the two-column selection marker).
const MODEL_PICKER_DETAIL_INDENT: &str = "    ";
/// Panel columns a single-line `/models` label cannot use: both border
/// columns, the scrollbar column and the two-column selection marker.
const MODEL_PICKER_CHROME_WIDTH: usize = 5;

/// Draw the slash-command overlay as a floating panel whose bottom edge sits
/// directly above the input box, so the list grows upward over the
/// conversation instead of pushing the input around while the user types.
pub(super) fn draw_slash_overlay(
    frame: &mut Frame,
    input_area: Rect,
    overlay: &SlashOverlay,
    drag: Option<BannerDragSelection>,
    engine_display: &crate::llm::engine::EngineDisplayInfo,
    overlay_layout: &mut SlashOverlayLayout,
) {
    let previous_first_visible = overlay_layout.first_visible_row;
    *overlay_layout = SlashOverlayLayout::default();
    // Visible choices are computed once per frame; the row layout and the
    // active-model styling both read this list.
    let visible_model_choices_in_overlay = match overlay {
        SlashOverlay::ModelPicker {
            choices, filter, ..
        } => visible_model_choices(choices, filter),
        _ => Vec::new(),
    };
    // A model takes two rows only when its single-line label would not fit
    // the panel, so wide terminals keep one row per model.
    // The parameter popup has one row per parameter, so it never splits rows.
    let row_layout = match overlay {
        SlashOverlay::ModelPicker {
            parameters: Some(_),
            ..
        } => PickerRowLayout::SingleRow,
        SlashOverlay::ModelPicker {
            choices, filter, ..
        } => model_picker_row_layout(
            choices,
            filter,
            (input_area.width as usize).saturating_sub(MODEL_PICKER_CHROME_WIDTH),
        ),
        _ => PickerRowLayout::SingleRow,
    };
    overlay_layout.row_layout = row_layout;
    let (title, selected) = match overlay {
        SlashOverlay::Hidden => return,
        SlashOverlay::Suggestions { selected, .. } => ("Commands".to_string(), Some(*selected)),
        SlashOverlay::LoadingModels => {
            ("Models  (querying provider connections…)".to_string(), None)
        }
        SlashOverlay::SessionPicker(picker) => (
            "Sessions".to_string(),
            picker.selected().map(|_| picker.selected),
        ),
        SlashOverlay::ModelPicker {
            choices,
            filter,
            selected,
            parameters,
        } => match parameters {
            Some(edit) => {
                let identity = choices
                    .get(edit.choice_index)
                    .map(|choice| choice.label.identity.clone())
                    .unwrap_or_default();
                (
                    format!("Model parameters  ·  {identity}"),
                    Some(edit.cursor),
                )
            }
            None => {
                let visible_indices = matching_model_indices(choices, filter);
                if visible_indices.is_empty() {
                    ("Engines & Provider Models".to_string(), None)
                } else {
                    ("Engines & Provider Models".to_string(), Some(*selected))
                }
            }
        },
    };
    let entries = if let SlashOverlay::SessionPicker(picker) = overlay {
        if picker.choices.is_empty() {
            vec![picker.status()]
        } else {
            picker
                .choices
                .iter()
                .map(|choice| session_row(choice, usize::from(input_area.width.saturating_sub(5))))
                .collect()
        }
    } else {
        slash_overlay_text_rows(overlay, row_layout)
    };
    if entries.is_empty() || input_area.width < 4 || input_area.y < 3 {
        return;
    }

    // Map each rendered row to the entry it shows, or `None` for the
    // divider row inserted between the engine-shortcut and provider/model
    // sections.
    let rows = match overlay {
        SlashOverlay::ModelPicker {
            parameters: Some(_),
            ..
        } => (0..entries.len()).map(Some).collect(),
        SlashOverlay::ModelPicker {
            choices, filter, ..
        } if !matching_model_indices(choices, filter).is_empty() => {
            model_picker_row_entries(choices, filter, row_layout)
        }
        _ => (0..entries.len()).map(Some).collect(),
    };

    // Cap the panel at two thirds of the space above the input so a provider
    // with hundreds of models cannot hide the conversation entirely.
    let max_content_height = (input_area.y.saturating_sub(1) * 2 / 3).max(1);
    let detail_lines = if let SlashOverlay::SessionPicker(picker) = overlay {
        let mut details = Vec::new();
        if input_area.y >= 4 {
            let button_style = |action| {
                if picker.action == action {
                    Style::default()
                        .fg(LIST_SELECTION_FOREGROUND_COLOR)
                        .bg(LIST_SELECTION_BACKGROUND_COLOR)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(LIST_TEXT_COLOR).bg(MUTED_TEXT_COLOR)
                }
            };
            details.push(Line::from(vec![
                Span::styled(
                    SESSION_RESUME_BUTTON_LABEL,
                    button_style(crate::tui_reducer_state::SessionAction::Resume),
                ),
                Span::raw(SESSION_ACTION_BUTTON_GAP),
                Span::styled(
                    SESSION_CLONE_BUTTON_LABEL,
                    button_style(crate::tui_reducer_state::SessionAction::Clone),
                ),
            ]));
            let status = picker.status();
            if !picker.choices.is_empty() && !status.is_empty() {
                details.push(Line::from(status));
            }
        }
        details
    } else {
        model_picker_detail_lines(
            overlay,
            engine_display,
            input_area.width.saturating_sub(4) as usize,
            max_content_height,
        )
    };
    let detail_height = saturating_u16(detail_lines.len());
    let detail_separator_height =
        u16::from(detail_height > 0 && !matches!(overlay, SlashOverlay::SessionPicker(_)));
    let list_max_height = max_content_height
        .saturating_sub(detail_height + detail_separator_height)
        .max(1);
    let mut content_height = saturating_u16(rows.len()).min(list_max_height);
    if matches!(overlay, SlashOverlay::SessionPicker(_)) {
        content_height = content_height.min(
            input_area
                .y
                .saturating_sub(detail_height + detail_separator_height + 2),
        );
    }
    let panel_height = content_height + detail_height + detail_separator_height + 2;
    let panel_area = Rect {
        x: input_area.x,
        y: input_area.y.saturating_sub(panel_height),
        width: input_area.width,
        height: panel_height,
    };

    // Keep the highlighted row inside the visible window when the list is
    // longer than the panel, but preserve the current window when the
    // highlighted row is already visible. This prevents a mouse click from
    // repositioning the popup unnecessarily.
    // A selected entry may span two rows; keep its whole span in view.
    let selected_span = selected.and_then(|selected| {
        let first_row = rows.iter().position(|row| *row == Some(selected))?;
        let last_row = rows.iter().rposition(|row| *row == Some(selected))?;
        Some((first_row, last_row))
    });
    let max_first_visible = rows.len().saturating_sub(content_height as usize);
    let mut first_visible = previous_first_visible.min(max_first_visible);
    if let Some((selected_first_row, selected_last_row)) = selected_span {
        let visible_end = first_visible.saturating_add(content_height as usize);
        if selected_first_row < first_visible {
            first_visible = selected_first_row;
        } else if selected_last_row >= visible_end {
            first_visible = (selected_last_row + 1)
                .saturating_sub(content_height as usize)
                .min(max_first_visible);
        }
    }
    let content_width = panel_area.width.saturating_sub(3) as usize;
    let mut lines: Vec<Line<'static>> = rows
        .iter()
        .enumerate()
        .skip(first_visible)
        .take(content_height as usize)
        .map(|(row_index, entry_index)| {
            let Some(_) = entry_index else {
                return Line::styled(
                    "─".repeat(content_width),
                    Style::default().fg(MUTED_TEXT_COLOR),
                );
            };
            let entry = &entries[row_index];
            let is_selected = *entry_index == selected;
            let is_model_details_row = matches!(overlay, SlashOverlay::ModelPicker { .. })
                && model_picker_row_is_details(&rows, row_index);
            let is_model_identity_row = matches!(overlay, SlashOverlay::ModelPicker { .. })
                && row_layout == PickerRowLayout::TwoRows
                && !is_model_details_row;
            // Active styling belongs to the model list; the parameter popup's
            // rows index its own fields, not the list.
            let list_is_shown = !matches!(
                overlay,
                SlashOverlay::ModelPicker {
                    parameters: Some(_),
                    ..
                }
            );
            let is_active_model_row = list_is_shown
                && !is_model_details_row
                && entry_index.is_some_and(|entry| {
                    visible_model_choices_in_overlay
                        .get(entry)
                        .is_some_and(|choice| choice.is_active)
                });
            let style = if is_selected {
                Style::default()
                    .fg(LIST_SELECTION_FOREGROUND_COLOR)
                    .bg(LIST_SELECTION_BACKGROUND_COLOR)
                    .add_modifier(Modifier::BOLD)
            } else if is_active_model_row {
                Style::default()
                    .fg(ACTIVE_MODEL_COLOR)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(LIST_TEXT_COLOR)
            };
            if is_model_details_row {
                let indent_width = MODEL_PICKER_DETAIL_INDENT.chars().count();
                let text =
                    truncate_with_ellipsis(entry, content_width.saturating_sub(indent_width));
                return Line::styled(format!("{MODEL_PICKER_DETAIL_INDENT}{text}"), style);
            }
            let marker = if is_selected { "▸ " } else { "  " };
            let text = if is_model_identity_row {
                elide_balanced_tokens(entry, content_width.saturating_sub(marker.chars().count()))
            } else {
                entry.clone()
            };
            Line::styled(format!("{marker}{text}"), style)
        })
        .collect();

    frame.render_widget(Clear, panel_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(LIST_BORDER_COLOR))
        .title(title);
    let inner_area = block.inner(panel_area);
    frame.render_widget(block, panel_area);

    let [list_area, detail_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(content_height),
            Constraint::Length(detail_height + detail_separator_height),
        ])
        .areas(inner_area);
    let [content_area, scrollbar_area] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Fill(1), Constraint::Length(1)])
        .areas(list_area);
    overlay_layout.content_area = content_area;
    overlay_layout.first_visible_row = first_visible;
    if let Some(selection) = drag {
        apply_overlay_drag_highlight(&mut lines, content_area, selection);
    }
    frame.render_widget(Paragraph::new(lines), content_area);

    if detail_height > 0 {
        let [separator_area, details_area] = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(detail_separator_height),
                Constraint::Length(detail_height),
            ])
            .areas(detail_area);
        frame.render_widget(
            Paragraph::new(Line::styled(
                "─".repeat(inner_area.width.saturating_sub(1) as usize),
                Style::default().fg(MUTED_TEXT_COLOR),
            )),
            separator_area,
        );
        if !matches!(overlay, SlashOverlay::SessionPicker(_)) {
            overlay_layout.detail_area = details_area;
            overlay_layout.detail_rows = detail_lines.iter().map(line_plain_text).collect();
        }
        let mut detail_lines = detail_lines;
        if let Some(selection) = drag {
            apply_overlay_drag_highlight(&mut detail_lines, details_area, selection);
        }
        frame.render_widget(
            Paragraph::new(detail_lines).style(Style::default().fg(MUTED_TEXT_COLOR)),
            details_area,
        );
        if matches!(overlay, SlashOverlay::SessionPicker(_)) {
            let resume_width = SESSION_RESUME_BUTTON_LABEL.len() as u16;
            let clone_offset = resume_width + SESSION_ACTION_BUTTON_GAP.len() as u16;
            overlay_layout.resume_action = Rect::new(
                details_area.x,
                details_area.y,
                details_area.width.min(resume_width),
                1,
            );
            overlay_layout.clone_action = Rect::new(
                details_area.x.saturating_add(clone_offset),
                details_area.y,
                details_area
                    .width
                    .saturating_sub(clone_offset)
                    .min(SESSION_CLONE_BUTTON_LABEL.len() as u16),
                1,
            );
        }
    }

    if rows.len() > content_height as usize {
        render_chat_scrollbar(
            frame,
            scrollbar_area,
            first_visible.min(max_first_visible),
            max_first_visible,
            rows.len(),
            content_height as usize,
            CHAT_SCROLLBAR_COLOR,
        );
    }
}

/// Shorten `text` to at most `width` characters, ending a cut text with `…`.
fn truncate_with_ellipsis(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let kept: String = text.chars().take(width - 1).collect();
    format!("{kept}…")
}

fn session_row(choice: &crate::tui_reducer_state::SessionChoice, width: usize) -> String {
    let prefix = format!("{}  ", choice.last_used_at);
    let title_width = width.saturating_sub(Span::raw(&prefix).width());
    if Span::raw(&choice.title).width() <= title_width {
        return format!("{prefix}{}", choice.title);
    }
    if title_width == 0 {
        return prefix;
    }
    let mut title = String::new();
    let mut used = 0;
    for character in choice.title.chars() {
        let character_width = Span::raw(character.to_string()).width();
        if used + character_width >= title_width {
            break;
        }
        title.push(character);
        used += character_width;
    }
    format!("{prefix}{title}…")
}

/// Concatenate a line's spans into the plain text the user sees on screen.
fn line_plain_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

/// Paint the drag selection onto the rows of `lines` that `area` displays.
/// Rows outside the selection's row span are left untouched.
fn apply_overlay_drag_highlight(
    lines: &mut [Line<'static>],
    area: Rect,
    selection: BannerDragSelection,
) {
    for (visible_index, line) in lines.iter_mut().enumerate() {
        let screen_row = area.y.saturating_add(saturating_u16(visible_index));
        if screen_row < selection.start_row || screen_row > selection.end_row {
            continue;
        }
        let column_start = if screen_row == selection.start_row {
            selection.start_col.saturating_sub(area.x) as usize
        } else {
            0
        };
        let column_end = if screen_row == selection.end_row {
            selection.end_col.saturating_sub(area.x) as usize
        } else {
            usize::MAX
        };
        apply_selection_highlight_to_line(line, column_start, column_end, OVERLAY_SELECTION_COLOR);
    }
}

fn model_picker_detail_lines(
    overlay: &SlashOverlay,
    engine_display: &crate::llm::engine::EngineDisplayInfo,
    width: usize,
    available_height: u16,
) -> Vec<Line<'static>> {
    let SlashOverlay::ModelPicker {
        choices,
        filter,
        selected,
        parameters,
    } = overlay
    else {
        return Vec::new();
    };
    if width < 40 || available_height < 3 || matching_model_indices(choices, filter).is_empty() {
        return Vec::new();
    }

    let selected_choice_index = matching_model_indices(choices, filter)
        .get(*selected)
        .copied();
    let selected_choice = selected_choice_index.and_then(|choice_index| choices.get(choice_index));
    // An unconfirmed context-length edit in the parameter popup previews live;
    // Enter commits it and Esc discards it.
    let draft_context_length = match parameters {
        Some(edit)
            if edit.context_length_edited && Some(edit.choice_index) == selected_choice_index =>
        {
            edit.context_length
        }
        _ => None,
    };
    let compaction_effect = selected_choice.map_or_else(
        || Line::from("Select an engine or model to preview compaction."),
        |choice| {
            if let Some(error) = &choice.selection_preview_error {
                return Line::from(format!("Compaction effect unavailable: {error}"));
            }
            let Some(target) = &choice.selection_target else {
                return Line::from("Compaction effect preview unavailable.");
            };
            let Some(compaction) = &engine_display.compaction_engine else {
                return Line::from("Compaction is disabled.");
            };
            let target_is_compaction_engine = compaction.shares_conversation
                && (target.engine_name.as_deref() == Some(compaction.engine_name.as_str())
                    || (target.engine_name.is_none() && compaction.engine_name.is_empty()));
            let selection_becomes_compaction_engine =
                compaction.follows_conversation || target_is_compaction_engine;
            let target_context_length = draft_context_length.unwrap_or(target.context_length);
            let mismatch = !selection_becomes_compaction_engine
                && target_context_length != compaction.context_length;
            let (provider_connection, model, context_length) =
                if selection_becomes_compaction_engine {
                    (
                        target.provider_connection.as_str(),
                        target.model.as_str(),
                        target_context_length,
                    )
                } else {
                    (
                        compaction.provider_connection.as_str(),
                        compaction.model.as_str(),
                        compaction.context_length,
                    )
                };
            let color = if mismatch {
                COMPACTION_MISMATCH_COLOR
            } else {
                MUTED_TEXT_COLOR
            };
            Line::styled(
                format!(
                    "Compaction uses {provider_connection}/{model}, context: {}",
                    crate::llm::token_estimate::format_token_estimate(context_length)
                ),
                Style::default().fg(color),
            )
        },
    );
    if compaction_effect.width() > width {
        return Vec::new();
    }
    vec![compaction_effect]
}

#[expect(clippy::too_many_arguments)]
pub(super) fn draw_clarify_banner(
    frame: &mut Frame,
    area: Rect,
    question: &str,
    choices: Option<&Vec<String>>,
    context: Option<&str>,
    drag: Option<BannerDragSelection>,
    scroll_offset: &mut usize,
    content_height: usize,
) {
    let wrap_width = area.width as usize;
    let mut lines: Vec<Line> = vec![];
    let mut content_row: usize = 0;

    // Clamp scroll so we never display empty space at the bottom.
    let viewport_height = area.height as usize;
    let max_scroll = content_height.saturating_sub(viewport_height);
    *scroll_offset = (*scroll_offset).min(max_scroll);
    let effective_scroll = *scroll_offset;

    // Each line's visual (terminal) row depends on the scroll position.
    // Lines above the scroll offset are off-screen; they map to area.y
    // (which is off-screen for drag purposes since the user can't click there).
    let abs_row_for = |cr: usize| -> u16 {
        area.y
            .saturating_add(saturating_u16(cr.saturating_sub(effective_scroll)))
    };

    // Context block (dim) — rendered before the question when present.
    if let Some(ctx) = context {
        for md_line in render_markdown(ctx, wrap_width) {
            let abs_row = abs_row_for(content_row);
            let line = banner_markdown_line_with_selection(md_line, abs_row, area.x, drag, false);
            lines.push(line);
            content_row += 1;
        }
        // Spacer between context and question.
        lines.push(Line::default());
        content_row += 1;
    }

    // Question lines (bold).
    let question_wrapped = render_markdown(question, wrap_width);
    let question_is_empty = question_wrapped.is_empty();
    for md_line in question_wrapped {
        let abs_row = abs_row_for(content_row);
        lines.push(banner_markdown_line_with_selection(
            md_line, abs_row, area.x, drag, true,
        ));
        content_row += 1;
    }
    if question_is_empty {
        // Ensure at least one row when wrapping yields nothing.
        let abs_row = abs_row_for(content_row);
        lines.push(banner_line_with_selection(
            question.to_string(),
            abs_row,
            area.x,
            drag,
            true,
        ));
        content_row += 1;
    }

    // Choice lines.
    if let Some(choices) = choices {
        for (index, choice) in choices.iter().enumerate() {
            let abs_row = abs_row_for(content_row);
            let text = format!("{}. {}", index + 1, choice);
            lines.push(banner_line_with_selection(
                text, abs_row, area.x, drag, false,
            ));
            content_row += 1;
        }
    }
    let _ = content_row;

    frame.render_widget(
        Paragraph::new(lines).scroll((saturating_u16(effective_scroll), 0)),
        area,
    );

    // Scroll indicators at the right edge of the first / last visible row.
    let scroll_indicator_style = Style::default().fg(CLARIFY_BANNER_SCROLL_INDICATOR_COLOR);
    let indicator_col = area.x + area.width.saturating_sub(1);
    if effective_scroll > 0 && area.height > 0 {
        frame.render_widget(
            Span::styled("▲", scroll_indicator_style),
            Rect::new(indicator_col, area.y, 1, 1),
        );
    }
    if effective_scroll < max_scroll && area.height > 0 {
        frame.render_widget(
            Span::styled("▼", scroll_indicator_style),
            Rect::new(indicator_col, area.y + area.height - 1, 1, 1),
        );
    }
}

/// Wraps a Markdown-rendered [`Line`] (from [`render_markdown`]) with the
/// clarify-banner's base foreground color and bold/dim overlay, preserving
/// inline markdown styling (bold, inline code, ...). While the user is
/// actively dragging a selection over this row, falls back to a flattened
/// plain-text line so the existing character-range highlight in
/// [`banner_line_with_selection`] still applies — markdown styling is not
/// preserved for the duration of the drag.
fn banner_markdown_line_with_selection(
    md_line: Line<'static>,
    abs_row: u16,
    area_x: u16,
    drag: Option<BannerDragSelection>,
    bold: bool,
) -> Line<'static> {
    let overlay = if bold {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::DIM)
    };
    let in_drag_range = drag.is_some_and(|sel| abs_row >= sel.start_row && abs_row <= sel.end_row);
    let line = if in_drag_range {
        let text: String = md_line.spans.iter().map(|s| s.content.as_ref()).collect();
        banner_line_with_selection(text, abs_row, area_x, drag, bold)
    } else {
        md_line.patch_style(Style::default().fg(CLARIFY_BANNER_FG_COLOR))
    };
    line.patch_style(overlay)
}

/// Builds a [`Line`] with the dragged column range highlighted and the rest
/// in the normal banner style. `bold` applies the bold modifier to
/// un-selected spans (used for question lines).
fn banner_line_with_selection(
    text: String,
    abs_row: u16,
    area_x: u16,
    drag: Option<BannerDragSelection>,
    bold: bool,
) -> Line<'static> {
    let normal_style = if bold {
        Style::default()
            .fg(CLARIFY_BANNER_FG_COLOR)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(CLARIFY_BANNER_FG_COLOR)
    };
    let selected_style = if bold {
        Style::default()
            .fg(CLARIFY_BANNER_SELECTION_FG_COLOR)
            .bg(CLARIFY_BANNER_FG_COLOR)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(CLARIFY_BANNER_SELECTION_FG_COLOR)
            .bg(CLARIFY_BANNER_FG_COLOR)
    };

    let Some(sel) = drag else {
        return Line::styled(text, normal_style);
    };

    if abs_row < sel.start_row || abs_row > sel.end_row {
        return Line::styled(text, normal_style);
    }

    let chars: Vec<char> = text.chars().collect();
    let line_len = chars.len();

    // Column offsets relative to the area's left edge.
    let (sel_start, sel_end) = if sel.start_row == sel.end_row {
        // Single-row selection.
        let start = sel.start_col.saturating_sub(area_x) as usize;
        let end = sel.end_col.saturating_sub(area_x) as usize;
        (start.min(line_len), end.min(line_len))
    } else if abs_row == sel.start_row {
        let start = sel.start_col.saturating_sub(area_x) as usize;
        (start.min(line_len), line_len)
    } else if abs_row == sel.end_row {
        let end = sel.end_col.saturating_sub(area_x) as usize;
        (0, end.min(line_len))
    } else {
        (0, line_len)
    };

    if sel_start == sel_end {
        return Line::styled(text, normal_style);
    }

    let before: String = chars[..sel_start].iter().collect();
    let selected: String = chars[sel_start..sel_end].iter().collect();
    let after: String = chars[sel_end..].iter().collect();

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(3);
    if !before.is_empty() {
        spans.push(Span::styled(before, normal_style));
    }
    spans.push(Span::styled(selected, selected_style));
    if !after.is_empty() {
        spans.push(Span::styled(after, normal_style));
    }
    Line::from(spans)
}

/// Render a transient notification in the bottom-centre of the screen.
/// The popup is one line of text between its border rows and includes
/// horizontal padding around the message.
pub(super) fn draw_toast(frame: &mut Frame, area: Rect, message: &str) {
    let content = format!(" ✓ {message} ");
    let popup_width = saturating_u16(content.chars().count().saturating_add(2)).min(area.width);
    let popup_height = 3u16;
    let x = area.x + area.width.saturating_sub(popup_width + 1);
    let y = area.y + area.height.saturating_sub(popup_height + 1);
    let popup = Rect::new(x, y, popup_width, popup_height);

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().fg(TOAST_FG_COLOR).bg(TOAST_BG_COLOR));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    frame.render_widget(
        Paragraph::new(Line::styled(
            content,
            Style::default()
                .fg(TOAST_FG_COLOR)
                .bg(TOAST_BG_COLOR)
                .add_modifier(Modifier::BOLD),
        ))
        .alignment(Alignment::Center),
        inner,
    );
}

#[cfg(test)]
mod tests;
