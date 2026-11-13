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
    active_model_color, approve_background_color, approve_hover_background_color,
    chat_scrollbar_color, clarify_banner_fg_color, clarify_banner_scroll_indicator_color,
    clarify_banner_selection_fg_color, deny_background_color, deny_hover_background_color,
    list_border_color, list_selection_background_color, list_selection_foreground_color,
    list_text_color, muted_text_color, overlay_selection_color, toast_bg_color, toast_fg_color,
    warning_color,
};
use crate::tui_reducer_state::{
    matching_model_indices, model_picker_row_entries, model_picker_row_is_details,
    model_picker_row_layout, slash_overlay_text_rows, visible_model_choices, BannerDragSelection,
    ParameterField, ParameterGlyphHit, PickerRow, PickerRowLayout, PromptButton, SessionAction,
    SlashOverlay, SlashOverlayLayout, ENGINE_SAVE_BUTTON_COUNT, GREATER_VALUE_GLYPH,
    SMALLER_VALUE_GLYPH,
};

use super::chat::{apply_selection_highlight_to_line, render_chat_scrollbar};
use super::footer::{
    button_style, draw_go_to_agent_button, elide_balanced_tokens, GO_TO_AGENT_LABEL,
};
use super::ButtonAreas;

const SESSION_RESUME_BUTTON_LABEL: &str = " [ Resume ] ";
const SESSION_CLONE_BUTTON_LABEL: &str = " [ Clone ] ";
const SESSION_DELETE_BUTTON_LABEL: &str = " [ Delete ] ";
const SESSION_ACTION_BUTTON_GAP: &str = "  ";
/// A `/models` list row with the model name (the model ID after its last `/`)
/// in bold. The name is searched only within the identity part of the row;
/// `None` when it is not found, e.g. because eliding cut it.
fn model_name_emphasized_line<'a>(
    marker: &str,
    text: &str,
    choice: &crate::tui_reducer_state::ModelChoice,
    style: Style,
) -> Option<Line<'a>> {
    let model_id = &choice.selection.model.id;
    let model_name = model_id
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())?;
    let identity_end = (0..=choice.label.identity.len().min(text.len()))
        .rev()
        .find(|index| text.is_char_boundary(*index))?;
    let start = text[..identity_end].rfind(model_name)?;
    let end = start + model_name.len();
    Some(Line::from(vec![
        Span::styled(format!("{marker}{}", &text[..start]), style),
        Span::styled(model_name.to_string(), style.add_modifier(Modifier::BOLD)),
        Span::styled(text[end..].to_string(), style),
    ]))
}

/// Indent of a `/models` details row, aligning it under the identity text
/// (past the two-column selection marker) plus two further columns.
const MODEL_PICKER_DETAIL_INDENT: &str = "      ";
/// Indent of a `/models` group header, one column left of the selection marker.
const MODEL_PICKER_HEADER_INDENT: &str = " ";
/// Panel columns a single-line `/models` label cannot use: both border
/// columns, the scrollbar column and the two-column selection marker.
const MODEL_PICKER_CHROME_WIDTH: usize = 5;

/// Narrowest terminal width at which the parameters and save pages keep every
/// action readable; below it the popup only asks for a wider terminal.
const PARAMETER_PAGE_MINIMUM_WIDTH: usize = 20;

/// Replace an unusably narrow parameters/save page by resize guidance. The
/// draft and focus stay in the reducer state; Esc keeps working.
fn draw_resize_guidance(frame: &mut Frame, input_area: Rect) {
    let height = input_area.y.min(4);
    let area = Rect::new(
        input_area.x,
        input_area.y - height,
        input_area.width,
        height,
    );
    frame.render_widget(Clear, area);
    let text = format!(
        "Widen the terminal to at least {PARAMETER_PAGE_MINIMUM_WIDTH} columns. \
         Your edits are kept; Esc cancels."
    );
    frame.render_widget(
        Paragraph::new(text)
            .wrap(ratatui::widgets::Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL)),
        area,
    );
}

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
    mouse_pos: Option<(u16, u16)>,
) {
    let previous_first_visible = overlay_layout.first_visible_row;
    let previous_detail_first_visible_row = overlay_layout.detail_first_visible_row;
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
                let page = if edit.save_form.is_some() {
                    "Save engine"
                } else {
                    "Model parameters"
                };
                (format!("{page}  ·  {identity}"), Some(edit.cursor))
            }
            None => {
                let visible_indices = matching_model_indices(choices, filter);
                let title = if visible_indices.len() == choices.len() {
                    format!("Engines & Provider Models  ({})", choices.len())
                } else {
                    format!(
                        "Engines & Provider Models  ({}/{})",
                        visible_indices.len(),
                        choices.len()
                    )
                };
                if visible_indices.is_empty() {
                    (title, None)
                } else {
                    (title, Some(*selected))
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
    if matches!(
        overlay,
        SlashOverlay::ModelPicker {
            parameters: Some(_),
            ..
        }
    ) && usize::from(input_area.width) < PARAMETER_PAGE_MINIMUM_WIDTH
    {
        draw_resize_guidance(frame, input_area);
        return;
    }

    let save_form = match overlay {
        SlashOverlay::ModelPicker {
            parameters: Some(edit),
            ..
        } => edit.save_form.as_ref(),
        _ => None,
    };
    let selected = match save_form {
        Some(form) => Some(form.focus_entry(entries.len())),
        None => selected,
    };
    // Action buttons wrap by measured display width: consecutive buttons share
    // a row while they fit, and the first button of a row stands for the row.
    let button_indices: Vec<usize> = match overlay {
        SlashOverlay::ModelPicker {
            parameters: Some(edit),
            ..
        } => match save_form {
            Some(_) => {
                (entries.len().saturating_sub(ENGINE_SAVE_BUTTON_COUNT)..entries.len()).collect()
            }
            None => edit
                .fields
                .iter()
                .enumerate()
                .filter(|(_, field)| {
                    matches!(
                        field,
                        ParameterField::ResetToConfigured
                            | ParameterField::Save
                            | ParameterField::Apply
                    )
                })
                .map(|(index, _)| index)
                .collect(),
        },
        _ => Vec::new(),
    };
    let button_row_width = (input_area.width as usize).saturating_sub(5 + 2);
    let mut button_groups: Vec<Vec<usize>> = Vec::new();
    let mut group_width = 0;
    for index in &button_indices {
        let width = Span::raw(format!(" [ {} ] ", entries[*index])).width();
        match button_groups.last_mut() {
            Some(group) if group_width + 1 + width <= button_row_width => {
                group.push(*index);
                group_width += 1 + width;
            }
            _ => {
                button_groups.push(vec![*index]);
                group_width = width;
            }
        }
    }
    let button_leader = |entry_index: usize| {
        button_groups
            .iter()
            .find(|group| group.contains(&entry_index))
            .map_or(entry_index, |group| group[0])
    };
    // Map each rendered row to the entry it shows, or to the group header
    // that precedes a group of model-picker entries.
    let rows: Vec<PickerRow> = match overlay {
        SlashOverlay::ModelPicker {
            parameters: Some(_),
            ..
        } => (0..entries.len())
            .filter(|entry_index| button_leader(*entry_index) == *entry_index)
            .map(PickerRow::Entry)
            .collect(),
        SlashOverlay::ModelPicker {
            choices, filter, ..
        } if !matching_model_indices(choices, filter).is_empty() => {
            model_picker_row_entries(choices, filter, row_layout)
        }
        _ => (0..entries.len()).map(PickerRow::Entry).collect(),
    };

    // Cap the panel at two thirds of the space above the input so a provider
    // with hundreds of models cannot hide the conversation entirely.
    // The model picker instead takes every row above the input, minus its borders.
    let is_model_picker = matches!(overlay, SlashOverlay::ModelPicker { .. });
    let max_content_height = if is_model_picker {
        input_area.y.saturating_sub(2).max(1)
    } else {
        super::maximum_panel_content_height(input_area)
    };
    let detail_lines = if let SlashOverlay::SessionPicker(picker) = overlay {
        let mut details = Vec::new();
        if input_area.y >= 4 {
            details.push(session_button_line(picker, None));
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
    // In the model picker the list above dominates and keeps its size while the
    // selection moves: it gets a fixed share of the height (at least three
    // rows; exactly the controls on the parameters page), the details take
    // the rest and scroll (mouse wheel over the details) when longer.
    let mut detail_lines = detail_lines;
    let parameters_open = matches!(
        overlay,
        SlashOverlay::ModelPicker {
            parameters: Some(_),
            ..
        }
    );
    let mut detail_first_visible_row = 0;
    let detail_total_rows = detail_lines.len();
    let mut model_picker_list_height = 0;
    if is_model_picker {
        let list_rows = saturating_u16(rows.len());
        model_picker_list_height = if parameters_open {
            list_rows.min(max_content_height)
        } else {
            list_rows
                .min(max_content_height.saturating_sub(1) * 2 / 3)
                .max(MINIMUM_LIST_ROWS)
                .min(max_content_height)
        };
        let mut detail_capacity =
            usize::from(max_content_height.saturating_sub(model_picker_list_height + 1));
        if parameters_open {
            detail_capacity = detail_capacity.max(usize::from(MINIMUM_SCROLLED_DETAIL_ROWS));
        }
        if detail_lines.len() > detail_capacity {
            let maximum_first_row = detail_lines.len() - detail_capacity;
            detail_first_visible_row = previous_detail_first_visible_row.min(maximum_first_row);
            detail_lines = detail_lines
                .into_iter()
                .skip(detail_first_visible_row)
                .take(detail_capacity)
                .collect();
        }
        // Fill the remaining height so the dialog keeps its size while the
        // selection (and with it the details) changes.
        if !detail_lines.is_empty() {
            detail_lines.resize(detail_capacity.max(detail_lines.len()), Line::default());
        }
    }
    let detail_height = saturating_u16(detail_lines.len());
    let detail_separator_height =
        u16::from(detail_height > 0 && !matches!(overlay, SlashOverlay::SessionPicker(_)));
    let list_max_height = max_content_height
        .saturating_sub(detail_height + detail_separator_height)
        .max(1);
    let mut content_height = if is_model_picker {
        model_picker_list_height.min(list_max_height)
    } else {
        saturating_u16(rows.len()).min(list_max_height)
    };
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
        let selected = button_leader(selected);
        let first_row = rows
            .iter()
            .position(|row| *row == PickerRow::Entry(selected))?;
        let last_row = rows
            .iter()
            .rposition(|row| *row == PickerRow::Entry(selected))?;
        // A group's first entry keeps its header in view, unless the window is
        // too short for both: a span that never fits makes the window jump
        // between two positions on every frame.
        let first_row = if first_row > 0
            && matches!(rows[first_row - 1], PickerRow::Header(_))
            && last_row + 1 - (first_row - 1) <= content_height as usize
        {
            first_row - 1
        } else {
            first_row
        };
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
    let list_origin = (panel_area.x + 1, panel_area.y + 1);
    let mut parameter_glyphs: Vec<ParameterGlyphHit> = Vec::new();
    let mut parameter_buttons: Vec<(Rect, usize)> = Vec::new();
    let mut engine_save_controls: Vec<(Rect, crate::tui_reducer_state::EngineSaveFocus)> =
        Vec::new();
    let mut lines: Vec<Line<'static>> = rows
        .iter()
        .enumerate()
        .skip(first_visible)
        .take(content_height as usize)
        .map(|(row_index, row)| {
            let entry_index = match row {
                PickerRow::Header(_) => {
                    // Group headers are presentation only: never highlighted.
                    let header_text = truncate_with_ellipsis(
                        &entries[row_index],
                        content_width.saturating_sub(MODEL_PICKER_HEADER_INDENT.chars().count()),
                    );
                    let header_style = Style::default()
                        .fg(muted_text_color())
                        .add_modifier(Modifier::BOLD);
                    return Line::from(vec![
                        Span::styled(MODEL_PICKER_HEADER_INDENT, header_style),
                        Span::styled(header_text, header_style.add_modifier(Modifier::UNDERLINED)),
                    ]);
                }
                PickerRow::Entry(entry_index) => *entry_index,
            };
            let entry = if button_indices.is_empty() {
                &entries[row_index]
            } else {
                &entries[entry_index]
            };
            let is_selected = selected == Some(entry_index);
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
                && visible_model_choices_in_overlay
                    .get(entry_index)
                    .is_some_and(|choice| choice.is_active);
            let is_misconfigured_row = list_is_shown
                && visible_model_choices_in_overlay
                    .get(entry_index)
                    .is_some_and(|choice| !choice.configuration_warnings.is_empty());
            let style = if is_selected {
                Style::default()
                    .fg(list_selection_foreground_color())
                    .bg(list_selection_background_color())
                    .add_modifier(Modifier::BOLD)
            } else if is_misconfigured_row {
                let style = Style::default().fg(warning_color());
                if is_active_model_row {
                    style.add_modifier(Modifier::BOLD)
                } else {
                    style
                }
            } else if is_active_model_row {
                Style::default()
                    .fg(active_model_color())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(list_text_color())
            };
            if is_model_details_row {
                let indent_width = MODEL_PICKER_DETAIL_INDENT.chars().count();
                let text =
                    truncate_with_ellipsis(entry, content_width.saturating_sub(indent_width));
                return Line::styled(format!("{MODEL_PICKER_DETAIL_INDENT}{text}"), style);
            }
            let marker = if is_selected { "▸ " } else { "  " };
            let parameter_button_style = |is_enabled: bool, is_hovered: bool, is_focused: bool| {
                if is_enabled {
                    button_style(
                        approve_background_color(),
                        approve_hover_background_color(),
                        is_hovered,
                        is_focused,
                    )
                } else {
                    Style::default().fg(muted_text_color())
                }
            };
            if let SlashOverlay::ModelPicker {
                choices,
                parameters: Some(edit),
                ..
            } = overlay
            {
                let first_save_button = entries.len().saturating_sub(ENGINE_SAVE_BUTTON_COUNT);
                let button_enabled = |index: usize| match (save_form, edit.fields.get(index)) {
                    (Some(form), _) => {
                        (index >= first_save_button).then_some(form.buttons_enabled())
                    }
                    (None, Some(ParameterField::ResetToConfigured)) => Some(
                        choices
                            .get(edit.choice_index)
                            .is_some_and(|choice| edit.reset_enabled(choice)),
                    ),
                    (None, Some(ParameterField::Save | ParameterField::Apply)) => Some(true),
                    _ => None,
                };
                if let Some(group) = button_groups.iter().find(|group| group[0] == entry_index) {
                    let screen_row = list_origin.1 + (row_index - first_visible) as u16;
                    let mut column = list_origin.0 + marker.chars().count() as u16;
                    let available = content_width.saturating_sub(marker.chars().count());
                    let mut spans =
                        vec![Span::styled(marker, Style::default().fg(list_text_color()))];
                    for (position, button_index) in group.iter().enumerate() {
                        let is_enabled = button_enabled(*button_index).unwrap_or(false);
                        let label = truncate_with_ellipsis(
                            &format!(" [ {} ] ", entries[*button_index]),
                            available,
                        );
                        let label_width = Span::raw(label.clone()).width();
                        let hovered = is_enabled
                            && mouse_pos.is_some_and(|(mouse_column, mouse_row)| {
                                mouse_row == screen_row
                                    && mouse_column >= column
                                    && (mouse_column as usize) < column as usize + label_width
                            });
                        if is_enabled {
                            let area = Rect::new(column, screen_row, label_width as u16, 1);
                            match save_form {
                                Some(form) => {
                                    if let Some(focus) =
                                        form.entry_focus(*button_index, entries.len())
                                    {
                                        engine_save_controls.push((area, focus));
                                    }
                                }
                                None => parameter_buttons.push((area, *button_index)),
                            }
                        }
                        if position > 0 {
                            spans.push(Span::raw(" "));
                        }
                        spans.push(Span::styled(
                            label,
                            parameter_button_style(
                                is_enabled,
                                hovered,
                                selected == Some(*button_index),
                            ),
                        ));
                        column += label_width as u16 + 1;
                    }
                    return Line::from(spans);
                }
                if let Some(form) = save_form {
                    // Mode and name rows are pointer targets; the rest is text.
                    if let Some(focus) = form.entry_focus(entry_index, entries.len()) {
                        engine_save_controls.push((
                            Rect::new(
                                list_origin.0,
                                list_origin.1 + (row_index - first_visible) as u16,
                                content_width as u16,
                                1,
                            ),
                            focus,
                        ));
                    }
                    let text = truncate_with_ellipsis(
                        entry,
                        content_width.saturating_sub(marker.chars().count()),
                    );
                    let caret_column = form
                        .name_caret_column()
                        .filter(|column| entry_index == 1 && *column <= text.chars().count());
                    if let Some(caret_column) = caret_column {
                        // A highlighted cell marks the caret on the character
                        // under it, so the text itself never shifts.
                        let mut characters: Vec<char> = text.chars().collect();
                        characters.push(' ');
                        let before: String = characters[..caret_column].iter().collect();
                        let after: String = characters[caret_column + 1..].iter().collect();
                        return Line::from(vec![
                            Span::styled(format!("{marker}{before}"), style),
                            Span::styled(
                                characters[caret_column].to_string(),
                                // Inverse of the selected row, which the name row is.
                                Style::default()
                                    .fg(list_selection_background_color())
                                    .bg(list_selection_foreground_color()),
                            ),
                            Span::styled(after, style),
                        ]);
                    }
                    return Line::styled(format!("{marker}{text}"), style);
                }
            }
            let text = if is_model_identity_row {
                elide_balanced_tokens(entry, content_width.saturating_sub(marker.chars().count()))
            } else {
                entry.clone()
            };
            if matches!(
                overlay,
                SlashOverlay::ModelPicker {
                    parameters: Some(_),
                    ..
                }
            ) {
                let screen_row = list_origin.1 + (row_index - first_visible) as u16;
                let glyph_columns: Vec<(usize, isize)> =
                    [(SMALLER_VALUE_GLYPH, -1), (GREATER_VALUE_GLYPH, 1)]
                        .into_iter()
                        .filter_map(|(glyph, delta)| {
                            let position = text
                                .chars()
                                .position(|character| character.to_string() == glyph)?;
                            Some((position, delta))
                        })
                        .collect();
                if !glyph_columns.is_empty() {
                    // The hit area spans the glyph and one cell on each side,
                    // so the pointer does not cover the whole highlight.
                    let padded_text = format!("{text} ");
                    let first_column = list_origin.0 + marker.chars().count() as u16;
                    let hovered_glyph = glyph_columns.iter().find(|(position, _)| {
                        let area = Rect::new(
                            first_column + position.saturating_sub(1) as u16,
                            screen_row,
                            3,
                            1,
                        );
                        mouse_pos.is_some_and(|(column, row)| {
                            area.contains(ratatui::layout::Position::new(column, row))
                        })
                    });
                    let hover_style = Style::default()
                        .fg(crate::theme::button_text_color())
                        .bg(approve_hover_background_color())
                        .add_modifier(Modifier::BOLD);
                    let mut spans = vec![Span::styled(marker, style)];
                    for (position, character) in padded_text.chars().enumerate() {
                        let is_highlighted = hovered_glyph.is_some_and(|(glyph_position, _)| {
                            position.abs_diff(*glyph_position) <= 1
                        });
                        spans.push(Span::styled(
                            character.to_string(),
                            if is_highlighted { hover_style } else { style },
                        ));
                    }
                    for (position, delta) in &glyph_columns {
                        parameter_glyphs.push(ParameterGlyphHit {
                            area: Rect::new(
                                first_column + position.saturating_sub(1) as u16,
                                screen_row,
                                3,
                                1,
                            ),
                            row: entry_index,
                            delta: *delta,
                        });
                    }
                    return Line::from(spans);
                }
            }
            if list_is_shown && !is_model_details_row {
                if let Some(choice) = visible_model_choices_in_overlay.get(entry_index) {
                    if let Some(line) = model_name_emphasized_line(marker, &text, choice, style) {
                        return line;
                    }
                }
            }
            Line::styled(format!("{marker}{text}"), style)
        })
        .collect();
    overlay_layout.parameter_glyphs = parameter_glyphs;
    overlay_layout.parameter_buttons = parameter_buttons;
    if !button_indices.is_empty() {
        overlay_layout.parameter_row_entries = rows
            .iter()
            .map(|row| match row {
                PickerRow::Entry(entry_index) => *entry_index,
                PickerRow::Header(_) => usize::MAX,
            })
            .collect();
    }
    overlay_layout.engine_save_controls = engine_save_controls;

    frame.render_widget(Clear, panel_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(list_border_color()))
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
                Style::default().fg(muted_text_color()),
            )),
            separator_area,
        );
        if !matches!(overlay, SlashOverlay::SessionPicker(_)) {
            overlay_layout.detail_area = details_area;
            overlay_layout.detail_first_visible_row = detail_first_visible_row;
            overlay_layout.detail_total_rows = detail_total_rows;
            overlay_layout.detail_rows = detail_lines.iter().map(line_plain_text).collect();
        }
        if let SlashOverlay::SessionPicker(picker) = overlay {
            let [resume_action, clone_action, delete_action] = session_button_areas(details_area);
            overlay_layout.resume_action = resume_action;
            overlay_layout.clone_action = clone_action;
            overlay_layout.delete_action = delete_action;
            let hovered_action = mouse_pos.and_then(|(column, row)| {
                [
                    (resume_action, SessionAction::Resume),
                    (clone_action, SessionAction::Clone),
                    (delete_action, SessionAction::Delete),
                ]
                .into_iter()
                .find(|(area, _)| area.contains((column, row).into()))
                .map(|(_, action)| action)
            });
            if hovered_action.is_some() {
                if let Some(first_line) = detail_lines.first_mut() {
                    *first_line = session_button_line(picker, hovered_action);
                }
            }
        }
        if let Some(selection) = drag {
            apply_overlay_drag_highlight(&mut detail_lines, details_area, selection);
        }
        frame.render_widget(
            Paragraph::new(detail_lines).style(Style::default().fg(muted_text_color())),
            details_area,
        );
        if detail_total_rows > usize::from(detail_height) {
            render_chat_scrollbar(
                frame,
                Rect {
                    x: details_area.right().saturating_sub(1),
                    width: 1,
                    ..details_area
                },
                detail_first_visible_row,
                detail_total_rows - usize::from(detail_height),
                detail_total_rows,
                usize::from(detail_height),
                chat_scrollbar_color(),
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
            chat_scrollbar_color(),
        );
    }
}

/// Screen rects of the Resume, Clone and Delete buttons in the button row.
fn session_button_areas(row_area: Rect) -> [Rect; 3] {
    let gap = SESSION_ACTION_BUTTON_GAP.len() as u16;
    let mut offset = 0u16;
    [
        SESSION_RESUME_BUTTON_LABEL,
        SESSION_CLONE_BUTTON_LABEL,
        SESSION_DELETE_BUTTON_LABEL,
    ]
    .map(|label| {
        let width = label.len() as u16;
        let area = Rect::new(
            row_area.x.saturating_add(offset),
            row_area.y,
            row_area.width.saturating_sub(offset).min(width),
            1,
        );
        offset = offset.saturating_add(width + gap);
        area
    })
}

/// The Resume / Clone / Delete button row. The focused button is filled;
/// the hovered one is lighter, and focus always wins over hover.
fn session_button_line(
    picker: &crate::tui_reducer_state::SessionPicker,
    hovered: Option<SessionAction>,
) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, (action, label)) in [
        (SessionAction::Resume, SESSION_RESUME_BUTTON_LABEL),
        (SessionAction::Clone, SESSION_CLONE_BUTTON_LABEL),
        (SessionAction::Delete, SESSION_DELETE_BUTTON_LABEL),
    ]
    .into_iter()
    .enumerate()
    {
        if index > 0 {
            spans.push(Span::raw(SESSION_ACTION_BUTTON_GAP));
        }
        let (normal, hover) = if action == SessionAction::Delete {
            (deny_background_color(), deny_hover_background_color())
        } else {
            (approve_background_color(), approve_hover_background_color())
        };
        spans.push(Span::styled(
            label,
            button_style(
                normal,
                hover,
                hovered == Some(action),
                picker.action == action,
            ),
        ));
    }
    Line::from(spans)
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
        apply_selection_highlight_to_line(
            line,
            column_start,
            column_end,
            overlay_selection_color(),
        );
    }
}

/// Fewest detail rows kept on the parameters page, however many controls it has.
const MINIMUM_SCROLLED_DETAIL_ROWS: u16 = 3;
/// Fewest list rows the model picker keeps while its details scroll.
const MINIMUM_LIST_ROWS: u16 = 3;

/// Wrap a single-style line to `width` columns instead of dropping it.
fn wrap_styled_line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    let style = line.style;
    textwrap::wrap(&line.to_string(), width.max(1))
        .into_iter()
        .map(|part| Line::styled(part.into_owned(), style))
        .collect()
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
    if width == 0 || available_height < 3 || matching_model_indices(choices, filter).is_empty() {
        return Vec::new();
    }
    let save_info: Vec<Line<'static>> = parameters
        .as_ref()
        .and_then(|edit| edit.save_form.as_deref())
        .map(|form| {
            let lines: Vec<Line<'static>> = form
                .info_lines()
                .iter()
                .flat_map(|text| {
                    let (indent, body) = match text.strip_prefix("  ") {
                        Some(body) => ("  ", body),
                        None => ("", text.as_str()),
                    };
                    let is_problem = text.starts_with('✗');
                    textwrap::wrap(body, width.saturating_sub(indent.len()).max(1))
                        .into_iter()
                        .map(|part| {
                            let line = format!("{indent}{part}");
                            if is_problem {
                                Line::styled(line, Style::default().fg(warning_color()))
                            } else {
                                Line::from(line)
                            }
                        })
                        .collect::<Vec<_>>()
                })
                .collect();
            lines
        })
        .unwrap_or_default();

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
                warning_color()
            } else {
                muted_text_color()
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
    let draft_reasoning_effort = match parameters {
        Some(edit) if Some(edit.choice_index) == selected_choice_index => {
            edit.reasoning_effort.as_deref()
        }
        _ => None,
    };
    let mut lines = save_info;
    // The list row may be elided on a narrow terminal, so spell out the identity here.
    if let Some(choice) = selected_choice {
        let identity_texts = choice
            .engine_name
            .iter()
            .map(|engine_name| format!("engine: {engine_name}"))
            .chain([
                format!("provider: {}", choice.selection.provider_connection),
                format!("model: {}", choice.selection.model.id),
            ])
            .chain((!choice.label.details.is_empty()).then(|| choice.label.details_text()));
        lines.extend(identity_texts.flat_map(|text| wrap_styled_line(Line::from(text), width)));
    }
    lines.extend(wrap_styled_line(compaction_effect, width));
    if let Some(choice) = selected_choice {
        lines.extend(
            crate::tui_reducer_state::selected_choice_detail_texts(choice, draft_reasoning_effort)
                .into_iter()
                .flat_map(|text| wrap_styled_line(Line::from(text), width)),
        );
    }
    lines
}

/// Draw the OK / Cancel buttons on `button_row` and return their rects.
pub(super) fn draw_clarify_buttons(
    frame: &mut Frame,
    button_row: Rect,
    focus: Option<PromptButton>,
    offers_go_to_agent: bool,
    mouse_pos: Option<(u16, u16)>,
) -> ButtonAreas {
    let ok_label = " [ ✓ OK ] ";
    let cancel_label = " [ ✗ Cancel ] ";
    let ok_width = saturating_u16(ok_label.chars().count());
    let cancel_width = saturating_u16(cancel_label.chars().count());
    let go_to_width = if offers_go_to_agent {
        saturating_u16(GO_TO_AGENT_LABEL.chars().count())
    } else {
        0
    };
    let [_, ok_area, _, cancel_area, _, go_to_area, _] = Layout::horizontal([
        Constraint::Length(1),
        Constraint::Length(ok_width),
        Constraint::Length(2),
        Constraint::Length(cancel_width),
        Constraint::Length(if offers_go_to_agent { 2 } else { 0 }),
        Constraint::Length(go_to_width),
        Constraint::Fill(1),
    ])
    .areas(button_row);
    let hovered = |area: Rect| {
        mouse_pos
            .is_some_and(|(column, row)| area.contains(ratatui::layout::Position::new(column, row)))
    };
    frame.render_widget(
        Paragraph::new(Line::styled(
            ok_label,
            button_style(
                approve_background_color(),
                approve_hover_background_color(),
                hovered(ok_area),
                focus == Some(PromptButton::ClarifyOk),
            ),
        )),
        ok_area,
    );
    frame.render_widget(
        Paragraph::new(Line::styled(
            cancel_label,
            button_style(
                deny_background_color(),
                deny_hover_background_color(),
                hovered(cancel_area),
                focus == Some(PromptButton::ClarifyCancel),
            ),
        )),
        cancel_area,
    );
    if offers_go_to_agent {
        draw_go_to_agent_button(
            frame,
            go_to_area,
            focus == Some(PromptButton::GoToAgent),
            hovered(go_to_area),
        );
    }
    ButtonAreas {
        approve: ok_area,
        deny: cancel_area,
        go_to_agent: if offers_go_to_agent {
            go_to_area
        } else {
            Rect::default()
        },
    }
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
    let scroll_indicator_style = Style::default().fg(clarify_banner_scroll_indicator_color());
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
        md_line.patch_style(Style::default().fg(clarify_banner_fg_color()))
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
            .fg(clarify_banner_fg_color())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(clarify_banner_fg_color())
    };
    let selected_style = if bold {
        Style::default()
            .fg(clarify_banner_selection_fg_color())
            .bg(clarify_banner_fg_color())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(clarify_banner_selection_fg_color())
            .bg(clarify_banner_fg_color())
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
        .style(Style::default().fg(toast_fg_color()).bg(toast_bg_color()));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    frame.render_widget(
        Paragraph::new(Line::styled(
            content,
            Style::default()
                .fg(toast_fg_color())
                .bg(toast_bg_color())
                .add_modifier(Modifier::BOLD),
        ))
        .alignment(Alignment::Center),
        inner,
    );
}

#[cfg(test)]
mod tests;
