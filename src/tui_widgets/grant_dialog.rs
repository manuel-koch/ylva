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

//! Grant dialog rendering: the proposed rules, the scope choice, the notes
//! to read and the Confirm / Back buttons, centred over the approval prompt.

use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::theme::{
    approve_background_color, approve_hover_background_color, chat_scrollbar_color, content_color,
    muted_text_color, navigation_background_color, navigation_hover_background_color,
    popup_border_color, popup_heading_color, tool_bubble_foreground_color, warning_color,
};
use crate::tui_reducer_state::{
    GrantDialogFocus, GrantDialogLayout, GrantDialogState, GrantRowSelection, GREATER_VALUE_GLYPH,
    SMALLER_VALUE_GLYPH,
};
use crate::tui_render_state::TuiState;

use super::chat::render_chat_scrollbar;
use super::footer::button_style;

const CONFIRM_LABEL: &str = " [ ✓ Grant ] ";
const RELOAD_LABEL: &str = " [ ⟳ Reload ] ";
const BACK_LABEL: &str = " [ ← Back ] ";

const COMMAND_RULE_HINT: &str = "Use * to match one or more arguments.";

fn is_command_row(row: &crate::tui_reducer_state::GrantDialogRow) -> bool {
    row.draft.tool.name() == crate::agent::tools::bash::BashTool::NAME
}

fn heading(text: &str) -> Line<'static> {
    Line::styled(
        text.to_string(),
        Style::default()
            .fg(popup_heading_color())
            .add_modifier(Modifier::BOLD),
    )
}

fn muted(text: String) -> Line<'static> {
    Line::styled(text, Style::default().fg(muted_text_color()))
}

/// The sections of the dialog body. `row_offsets` are the indices within
/// `rules` of the first line of each grant row.
struct Sections {
    rules: Vec<Line<'static>>,
    row_offsets: Vec<usize>,
    /// Number of lines of each row's editable rule, directly below its first line.
    rule_line_counts: Vec<usize>,
    /// Body line and dialog column of the caret of the rule being edited.
    caret: Option<(usize, usize)>,
    scopes: Vec<Line<'static>>,
    notes: Vec<Line<'static>>,
}

fn build_sections(dialog: &GrantDialogState, text_width: usize) -> Sections {
    let plain = |text: String| Line::styled(text, Style::default().fg(content_color()));

    let mut rules = vec![heading(if dialog.preview.refusal.is_some() {
        "Cannot be granted:"
    } else {
        "Proposed rules:"
    })];
    let mut row_offsets = Vec::new();
    let mut rule_line_counts = Vec::new();
    let mut caret = None;
    match &dialog.preview.refusal {
        Some(reason) => rules.extend(
            crate::text_wrap::wrap(reason, text_width)
                .into_iter()
                .map(plain),
        ),
        None => {
            for (index, row) in dialog.rows.iter().enumerate() {
                let focused = dialog.focused == GrantDialogFocus::Row(index);
                let edit = dialog.editing.as_ref().filter(|edit| edit.row == index);
                let editing = edit.is_some();
                let rule = match &row.draft.rule {
                    Some(rule) => rule.clone(),
                    None => "(any call)".to_string(),
                };
                let name = row
                    .draft
                    .name
                    .as_ref()
                    .map(|name| format!(" {name}"))
                    .unwrap_or_default();
                if index > 0 {
                    rules.push(Line::default());
                }
                row_offsets.push(rules.len());
                let base = if focused {
                    Style::default()
                        .fg(content_color())
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(content_color())
                };
                let tool_style = Style::default()
                    .fg(tool_bubble_foreground_color())
                    .add_modifier(if focused {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    });
                let spans = vec![
                    Span::styled(if focused { "▸ " } else { "  " }, base),
                    Span::styled(row.draft.tool.name().to_string(), tool_style),
                    Span::styled(format!("{name}  "), base),
                    Span::styled(format!("{SMALLER_VALUE_GLYPH} "), base),
                    Span::styled(format!("{:<10}", row.selection.label()), base),
                    Span::styled(format!(" {GREATER_VALUE_GLYPH}"), base),
                ];
                rules.push(Line::from(spans));
                let rule_prefix = "    ✎ ";
                let rule_start = rules.len();
                let rule_width = text_width.saturating_sub(rule_prefix.chars().count());
                // While editing, characters keep their cells so the terminal
                // cursor can sit exactly at the insertion point.
                let rule_lines = match (edit, &row.draft.rule) {
                    (Some(edit), Some(rule)) => {
                        let width = rule_width.max(1);
                        let cursor = edit.cursor.min(rule.chars().count());
                        caret = Some((
                            rules.len() + cursor / width,
                            rule_prefix.chars().count() + cursor % width,
                        ));
                        crate::text_wrap::indent_lines(
                            crate::text_wrap::character_wrap(rule, width),
                            rule_prefix,
                        )
                    }
                    _ => crate::text_wrap::indented(&rule, rule_prefix, text_width),
                };
                for (line_index, line) in rule_lines.into_iter().enumerate() {
                    let split = rule_prefix.chars().count();
                    let (prefix, text): (String, String) = (
                        line.chars().take(split).collect(),
                        line.chars().skip(split).collect(),
                    );
                    let mut line_spans = vec![
                        Span::styled(prefix, Style::default().fg(muted_text_color())),
                        Span::styled(text, base),
                    ];
                    if line_index == 0 && focused && !editing && row.draft.rule.is_some() {
                        line_spans.push(Span::styled(
                            "  enter: edit",
                            Style::default().fg(muted_text_color()),
                        ));
                    }
                    rules.push(Line::from(line_spans));
                }
                rule_line_counts.push(if row.draft.rule.is_some() {
                    rules.len() - rule_start
                } else {
                    0
                });
                if editing && is_command_row(row) {
                    rules.extend(
                        crate::text_wrap::indented(COMMAND_RULE_HINT, "      ", text_width)
                            .into_iter()
                            .map(muted),
                    );
                }
                if let Some(error) = dialog.row_error(index) {
                    rules.extend(
                        crate::text_wrap::indented(
                            &format!("Invalid: {error}"),
                            "      ✗ ",
                            text_width,
                        )
                        .into_iter()
                        .map(|line| Line::styled(line, Style::default().fg(warning_color()))),
                    );
                }
            }
            let already_allowed = dialog.preview.already_allowed;
            if already_allowed > 0 {
                let noun = if already_allowed == 1 {
                    "command is"
                } else {
                    "commands are"
                };
                rules.extend(
                    crate::text_wrap::indented(
                        &format!("{already_allowed} more {noun} already allowed by policy or an existing grant."),
                        "  ",
                        text_width,
                    )
                    .into_iter()
                    .map(muted),
                );
            }
            if dialog.preview.other_parts {
                rules.extend(
                    crate::text_wrap::indented(
                        "Other parts of this call can only be approved once.",
                        "  ",
                        text_width,
                    )
                    .into_iter()
                    .map(muted),
                );
            }
        }
    }

    let mut scopes = Vec::new();
    if dialog.preview.refusal.is_none() {
        scopes.push(heading("Scope:"));
        for (label, meaning) in [
            ("one-time:", "grant for this call only"),
            ("temporary:", "grant until the conversation is cleared"),
            (
                "persistent:",
                "grant and save decision to this profile's grants.toml",
            ),
        ] {
            scopes.extend(
                crate::text_wrap::indented(meaning, &format!("  {label:<12}"), text_width)
                    .into_iter()
                    .map(muted),
            );
        }
        if let Some(reason) = &dialog.preview.persistent_unavailable {
            scopes.extend(
                crate::text_wrap::indented(
                    &format!("Persistent unavailable: {reason}"),
                    "  ✗ ",
                    text_width,
                )
                .into_iter()
                .map(|line| Line::styled(line, Style::default().fg(warning_color()))),
            );
        }
    }

    let mut notes: Vec<Line> = Vec::new();
    if let Some(failure) = &dialog.failure {
        notes.extend(
            crate::text_wrap::indented(
                &format!("Nothing was created: {}", failure.reason),
                " ✗ ",
                text_width,
            )
            .into_iter()
            .map(|line| Line::styled(line, Style::default().fg(warning_color()))),
        );
        let hint = if failure.reloadable && !failure.reloaded {
            "Reload accepts the file as it is now (grants in it stay inactive \
             in this run); Grant then saves the selection, or go Back to approve once."
        } else if failure.reloadable {
            "The file was accepted as it is now; Grant saves the selection, or \
             go Back to approve once."
        } else {
            "Fix or change the rows and retry, or go Back to approve once."
        };
        notes.extend(
            crate::text_wrap::indented(hint, "   ", text_width)
                .into_iter()
                .map(muted),
        );
    }
    if dialog.preview.refusal.is_none() {
        for warning in dialog.warnings() {
            notes.extend(
                crate::text_wrap::indented(&warning, " • ", text_width)
                    .into_iter()
                    .map(muted),
            );
        }
    }
    // Reserve the room a persistent selection adds, so changing a selection
    // never resizes the dialog.
    Sections {
        rules,
        row_offsets,
        rule_line_counts,
        caret,
        scopes,
        notes,
    }
}

struct Body {
    lines: Vec<Line<'static>>,
    row_offsets: Vec<usize>,
    rule_line_counts: Vec<usize>,
    caret: Option<(usize, usize)>,
}

fn build_body(dialog: &GrantDialogState, text_width: usize) -> Body {
    let sections = build_sections(dialog, text_width);
    let mut lines: Vec<Line> = sections.rules;
    if !sections.scopes.is_empty() {
        lines.push(Line::default());
        lines.extend(sections.scopes);
    }
    if !sections.notes.is_empty() {
        lines.push(Line::default());
        lines.push(heading("Notes:"));
        lines.extend(sections.notes);
    }
    Body {
        lines,
        row_offsets: sections.row_offsets,
        rule_line_counts: sections.rule_line_counts,
        caret: sections.caret,
    }
}

/// Fewest terminal rows the dialog is drawn in: borders, a few body rows and
/// the button row. Taller content scrolls.
const MINIMUM_DIALOG_HEIGHT: u16 = 7;

pub(super) fn grant_dialog_required_height() -> u16 {
    MINIMUM_DIALOG_HEIGHT
}

/// Draws the dialog above `input_area` and records its geometry in `state.grant_dialog_layout`.
/// The body scrolls; the button row stays pinned below it.
pub(super) fn draw_grant_dialog(
    frame: &mut Frame,
    state: &mut TuiState,
    input_area: Rect,
    mouse_pos: Option<(u16, u16)>,
) {
    let Some(dialog) = state.grant_dialog.clone() else {
        return;
    };
    // Like the slash-command panel: left-aligned with the input, bottom edge
    // directly above it, so only the space above the input is available.
    let area = Rect::new(input_area.x, 0, input_area.width, input_area.y);
    if grant_dialog_required_height() > area.height {
        state.grant_dialog_layout = GrantDialogLayout::default();
        return;
    }
    let width = area.width.max(1);
    // One column each side for padding; the right one carries the scrollbar.
    let text_width = usize::from(width.saturating_sub(4)).max(1);

    let Body {
        lines: mut body,
        row_offsets,
        rule_line_counts,
        caret,
    } = build_body(&dialog, text_width);
    // Reserve the lines a persistent selection or the edit hint adds, so
    // neither resizes the dialog.
    let hint_in_body = dialog
        .editing
        .as_ref()
        .is_some_and(|edit| dialog.rows.get(edit.row).is_some_and(is_command_row));
    let hint_room = if !hint_in_body && dialog.rows.iter().any(is_command_row) {
        crate::text_wrap::indented(COMMAND_RULE_HINT, "      ", text_width).len()
    } else {
        0
    };
    let mut reserved = body.len() + hint_room;
    if dialog.preview.persistent_unavailable.is_none() {
        let mut worst_case = dialog.clone();
        for row in &mut worst_case.rows {
            row.selection = GrantRowSelection::Persistent;
        }
        reserved = reserved.max(build_body(&worst_case, text_width).lines.len() + hint_room);
    }
    body.resize(reserved, Line::default());
    let height = (body.len() as u16 + 3).min(area.height);
    let body_rows = usize::from(height - 3);
    let maximum_scroll = body.len().saturating_sub(body_rows);
    let scroll = dialog.scroll_offset.min(maximum_scroll);
    let rect = Rect::new(
        area.x,
        area.bottom() - height,
        width.min(area.width),
        height,
    );

    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(popup_border_color()))
        .title(" Grant similar calls ");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let content = Rect::new(
        inner.x + 1.min(inner.width),
        inner.y,
        inner.width.saturating_sub(2),
        inner.height,
    );
    let body_area = Rect::new(content.x, content.y, content.width, body_rows as u16);
    let visible: Vec<Line> = body.into_iter().skip(scroll).take(body_rows).collect();
    frame.render_widget(
        Paragraph::new(visible).alignment(Alignment::Left),
        body_area,
    );
    if maximum_scroll > 0 {
        render_chat_scrollbar(
            frame,
            Rect::new(
                inner.right().saturating_sub(1),
                body_area.y,
                1,
                body_area.height,
            ),
            scroll,
            maximum_scroll,
            body_rows + maximum_scroll,
            body_rows,
            chat_scrollbar_color(),
        );
    }

    if let Some((body_line, column)) = caret {
        let visible_line = body_line
            .checked_sub(scroll)
            .filter(|line| *line < body_rows);
        if let (Some(line), true) = (visible_line, column < usize::from(body_area.width)) {
            frame.set_cursor_position((body_area.x + column as u16, body_area.y + line as u16));
        }
    }

    let row_rect =
        |body_row: usize| match body_row.checked_sub(scroll).filter(|row| *row < body_rows) {
            Some(row) => Rect::new(body_area.x, body_area.y + row as u16, body_area.width, 1),
            None => Rect::default(),
        };
    let buttons = Rect::new(content.x, body_area.bottom(), content.width, 1);
    let confirm_width = (CONFIRM_LABEL.chars().count() as u16).min(buttons.width);
    let confirm_button = Rect::new(buttons.x, buttons.y, confirm_width, buttons.height.min(1));
    let reload_button = if dialog
        .failure
        .as_ref()
        .is_some_and(|failure| failure.reloadable)
    {
        let reload_start = (buttons.x + confirm_width + 2).min(buttons.right());
        Rect::new(
            reload_start,
            buttons.y,
            (RELOAD_LABEL.chars().count() as u16).min(buttons.right() - reload_start),
            buttons.height.min(1),
        )
    } else {
        Rect::default()
    };
    let back_start = if reload_button.is_empty() {
        (buttons.x + confirm_width + 2).min(buttons.right())
    } else {
        (reload_button.right() + 2).min(buttons.right())
    };
    let back_button = Rect::new(
        back_start,
        buttons.y,
        (BACK_LABEL.chars().count() as u16).min(buttons.right() - back_start),
        buttons.height.min(1),
    );
    draw_buttons(
        frame,
        mouse_pos,
        &dialog,
        confirm_button,
        reload_button,
        back_button,
    );

    state.grant_dialog_layout = GrantDialogLayout {
        rect,
        rules: row_offsets
            .iter()
            .zip(&rule_line_counts)
            .map(|(offset, count)| {
                (0..*count)
                    .map(|line| row_rect(offset + 1 + line))
                    .filter(|area| !area.is_empty())
                    .collect()
            })
            .collect(),
        rows: row_offsets.into_iter().map(row_rect).collect(),
        confirm_button,
        reload_button,
        back_button,
        maximum_scroll,
        body_rows,
    };
}

fn draw_buttons(
    frame: &mut Frame,
    mouse_pos: Option<(u16, u16)>,
    dialog: &GrantDialogState,
    confirm: Rect,
    reload: Rect,
    back: Rect,
) {
    let pointer = mouse_pos.map(|(column, row)| ratatui::layout::Position::new(column, row));
    let hovered = |area: Rect| pointer.is_some_and(|position| area.contains(position));
    let confirm_style = if dialog.can_confirm() {
        button_style(
            approve_background_color(),
            approve_hover_background_color(),
            hovered(confirm),
            dialog.focused == GrantDialogFocus::Confirm,
        )
    } else {
        Style::default().fg(muted_text_color())
    };
    let reload_style = if dialog.can_reload() {
        button_style(
            approve_background_color(),
            approve_hover_background_color(),
            hovered(reload),
            dialog.focused == GrantDialogFocus::Reload,
        )
    } else {
        Style::default().fg(muted_text_color())
    };
    let back_style = button_style(
        navigation_background_color(),
        navigation_hover_background_color(),
        hovered(back),
        dialog.focused == GrantDialogFocus::Back,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(CONFIRM_LABEL, confirm_style))),
        confirm,
    );
    if !reload.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(RELOAD_LABEL, reload_style))),
            reload,
        );
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(BACK_LABEL, back_style))),
        back,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui_reducer_state::GrantDialogRow;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use uuid::Uuid;

    fn dialog(
        notes: usize,
        persistent_unavailable: Option<&str>,
        refusal: Option<&str>,
    ) -> GrantDialogState {
        let command = |words: &[&str]| crate::grants::CommandStatement::Command {
            command: crate::grants::CommandSubject {
                tokens: words
                    .iter()
                    .map(|word| crate::grants::CommandToken::new(*word, false))
                    .collect(),
                has_unquoted_expansion: false,
                has_variable_assignment_prefix: false,
            },
            policy_approved: false,
        };
        let draft = |rule: &str| crate::grants::GrantDraft {
            tool: crate::grants::GrantTool::Bash,
            name: None,
            rule: Some(rule.to_string()),
        };
        // Many rules make the body long, so a short terminal has to scroll.
        let rules: Vec<_> = (0..notes.max(1))
            .map(|index| {
                if index == 0 {
                    "make test".to_string()
                } else {
                    format!("tool{index} run")
                }
            })
            .collect();
        let statements = rules
            .iter()
            .map(|rule| command(&rule.split(' ').collect::<Vec<_>>()))
            .collect();
        let preview = crate::agent::GrantPreview {
            id: Uuid::new_v4(),
            rows: rules.iter().map(|rule| draft(rule)).collect(),
            already_allowed: 2,
            other_parts: true,
            refusal: refusal.map(str::to_string),
            persistent_unavailable: persistent_unavailable.map(str::to_string),
            marker_to_create: None,
            subjects: crate::grants::GrantSubjects::Commands(statements),
            workspace_root: std::path::PathBuf::from("/work"),
        };
        let rows = preview
            .rows
            .iter()
            .map(|draft| GrantDialogRow {
                draft: draft.clone(),
                selection: GrantRowSelection::Temporary,
            })
            .collect();
        GrantDialogState {
            preview,
            rows,
            focused: GrantDialogFocus::Confirm,
            editing: None,
            failure: None,
            awaiting_answer: false,
            scroll_offset: 0,
            rule_click: Default::default(),
        }
    }

    fn render(width: u16, height: u16, dialog: GrantDialogState) -> (String, GrantDialogLayout) {
        let mut state = TuiState::new();
        state.grant_dialog = Some(dialog);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| {
                draw_grant_dialog(frame, &mut state, Rect::new(0, height - 3, width, 3), None)
            })
            .expect("draw");
        let buffer = terminal.backend().buffer();
        let text = (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        (text, state.grant_dialog_layout)
    }

    #[test]
    fn the_dialog_shows_rows_selections_footnotes_and_buttons_with_hit_areas() {
        let (text, layout) = render(90, 30, dialog(2, None, None));
        assert_eq!(layout.maximum_scroll, 0);
        for expected in [
            "temporary",
            "◂ temporary",
            "✎ make test",
            "2 more commands are already allowed",
            "Other parts of this call can only be approved once.",
            "Notes:",
            "Grant ]",
            "Back ]",
        ] {
            assert!(text.contains(expected), "{expected}: {text}");
        }
        assert_eq!(layout.rows.len(), 2);
        assert!(layout.rows[0].y < layout.rows[1].y);
        assert!(layout.rows[1].y < layout.confirm_button.y);
        assert!(!layout.confirm_button.is_empty() && !layout.back_button.is_empty());
    }

    #[test]
    fn the_already_allowed_sentence_wraps_on_a_narrow_dialog() {
        let (text, _) = render(40, 40, dialog(2, None, None));
        assert!(text.contains("2 more commands are already"), "{text}");
        assert!(text.contains("allowed by policy or an existing"), "{text}");
    }

    #[test]
    fn editing_a_command_rule_hints_at_the_wildcard_without_resizing() {
        let (idle_text, idle) = render(90, 40, dialog(2, None, None));
        assert!(!idle_text.contains("Use * to match"), "{idle_text}");
        let mut editing = dialog(2, None, None);
        editing.editing = Some(crate::tui_reducer_state::GrantRowEdit {
            row: 0,
            cursor: 0,
            original: editing.rows[0].draft.rule.clone(),
        });
        let (text, edited) = render(90, 40, editing);
        assert!(
            text.contains("Use * to match one or more arguments."),
            "{text}"
        );
        assert_eq!(idle.rect.height, edited.rect.height);
    }

    #[test]
    fn the_terminal_cursor_sits_at_the_caret_without_shifting_the_rule() {
        let mut editing = dialog(1, None, None);
        let rule = editing.rows[0].draft.rule.clone().unwrap();
        editing.editing = Some(crate::tui_reducer_state::GrantRowEdit {
            row: 0,
            cursor: 2,
            original: Some(rule.clone()),
        });
        let mut state = TuiState::new();
        state.grant_dialog = Some(editing);
        let mut terminal = Terminal::new(TestBackend::new(90, 40)).expect("terminal");
        terminal
            .draw(|frame| draw_grant_dialog(frame, &mut state, Rect::new(0, 37, 90, 3), None))
            .expect("draw");
        let cursor = terminal.get_cursor_position().expect("cursor");
        let buffer = terminal.backend().buffer();
        let cells: Vec<String> = (0..90)
            .map(|x| buffer[(x, cursor.y)].symbol().to_string())
            .collect();
        let start = cells
            .windows(rule.chars().count())
            .position(|window| window.concat() == rule)
            .expect("rule is drawn unbroken") as u16;
        assert_eq!(cursor.x, start + 2);
    }

    #[test]
    fn a_long_rule_being_edited_wraps_by_character_and_keeps_the_caret_on_screen() {
        let mut editing = dialog(1, None, None);
        editing.rows[0].draft.rule = Some("make ".repeat(20).trim_end().to_string());
        let length = editing.rows[0].draft.rule.as_ref().unwrap().chars().count();
        editing.editing = Some(crate::tui_reducer_state::GrantRowEdit {
            row: 0,
            cursor: length,
            original: None,
        });
        let mut state = TuiState::new();
        state.grant_dialog = Some(editing);
        let mut terminal = Terminal::new(TestBackend::new(40, 40)).expect("terminal");
        terminal
            .draw(|frame| draw_grant_dialog(frame, &mut state, Rect::new(0, 37, 40, 3), None))
            .expect("draw");
        let cursor = terminal.get_cursor_position().expect("cursor");
        let layout = &state.grant_dialog_layout;
        let last_rule_line = layout.rules[0].last().expect("rule lines");
        assert_eq!(cursor.y, last_rule_line.y);
    }

    #[test]
    fn the_rule_lines_of_each_row_are_hit_areas() {
        let (_, layout) = render(90, 40, dialog(2, None, None));
        assert_eq!(layout.rules.len(), 2);
        assert!(layout.rules.iter().all(|areas| !areas.is_empty()));
        assert_eq!(layout.rules[0][0].y, layout.rows[0].y + 1);
    }

    #[test]
    fn changing_a_selection_does_not_resize_the_dialog() {
        let (_, temporary) = render(90, 40, dialog(2, None, None));
        let mut persistent = dialog(2, None, None);
        persistent.rows[0].selection = GrantRowSelection::Persistent;
        let (_, resized) = render(90, 40, persistent);
        assert_eq!(temporary.rect.height, resized.rect.height);
    }

    #[test]
    fn an_invalid_edit_and_a_failure_are_explained() {
        let mut edited = dialog(1, None, None);
        edited.rows[0].draft.rule = Some("git push".to_string());
        edited.rows[0].selection = GrantRowSelection::OneTime;
        edited.failure = Some(crate::tui_reducer_state::GrantFailure {
            reason: "file changed".to_string(),
            reloadable: true,
            reloaded: false,
        });
        let (text, _) = render(90, 30, edited);
        assert!(text.contains("Invalid:"), "{text}");
        assert!(text.contains("one-time"), "{text}");
        assert!(text.contains("Nothing was created: file changed"), "{text}");
        assert!(
            text.contains("Reload accepts the file as it is now"),
            "{text}"
        );
    }

    #[test]
    fn a_reloadable_failure_offers_the_reload_button_between_grant_and_back() {
        let mut failed = dialog(1, None, None);
        failed.failure = Some(crate::tui_reducer_state::GrantFailure {
            reason: "file changed".to_string(),
            reloadable: true,
            reloaded: false,
        });
        let (text, layout) = render(90, 30, failed);
        assert!(text.contains("⟳ Reload"), "{text}");
        assert!(
            !layout.reload_button.is_empty()
                && layout.confirm_button.right() < layout.reload_button.x,
            "reload sits right of Grant"
        );
        assert!(
            layout.reload_button.right() < layout.back_button.x,
            "Back sits right of Reload"
        );

        let (_, plain) = render(90, 30, dialog(1, None, None));
        assert!(
            plain.reload_button.is_empty(),
            "without a failure no reload button is drawn"
        );
    }

    #[test]
    fn a_used_reload_turns_muted_until_a_further_failure_re_offers_it() {
        let mut failed = dialog(1, None, None);
        failed.failure = Some(crate::tui_reducer_state::GrantFailure {
            reason: "file changed".to_string(),
            reloadable: true,
            reloaded: true,
        });
        // The button stays visible and in place but is no longer offered.
        let (text, layout) = render(90, 30, failed);
        assert!(text.contains("⟳ Reload"), "{text}");
        assert!(!layout.reload_button.is_empty());
        assert!(!text.contains("Reload accepts the file as it is now"));
    }

    #[test]
    fn a_short_terminal_scrolls_the_body_and_keeps_the_buttons() {
        let mut long = dialog(6, Some("no profile directory"), None);
        let (text, layout) = render(40, 12, long.clone());
        assert!(!layout.confirm_button.is_empty(), "{text}");
        assert!(layout.maximum_scroll > 0, "{text}");
        long.scroll_offset = usize::MAX;
        let (text, layout) = render(40, 12, long);
        assert!(!layout.confirm_button.is_empty(), "{text}");
        assert!(
            text.contains("Persistent unavailable") || text.contains("Notes"),
            "{text}"
        );
        assert!(layout.rows.iter().all(|row| row.is_empty()));
        let (_, layout) = render(
            40,
            grant_dialog_required_height() + 2,
            dialog(1, None, None),
        );
        assert_eq!(layout, GrantDialogLayout::default());
    }

    #[test]
    fn a_refused_grant_shows_the_reason_instead_of_rules() {
        let (text, _) = render(80, 24, dialog(1, None, Some("approve it once")));
        assert!(text.contains("Cannot be granted:") && text.contains("approve it once"));
        assert!(!text.contains("Proposed rules:"));
    }
}
