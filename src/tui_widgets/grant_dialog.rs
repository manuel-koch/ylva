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

//! Grant dialog rendering: the proposed rules, the lifetime choice, the notes
//! to read and the Confirm / Back buttons, centred over the approval prompt.

use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::grants::GrantLifetime;
use crate::theme::{
    approve_background_color, approve_hover_background_color, chat_scrollbar_color, content_color,
    deny_background_color, deny_hover_background_color, help_popup_border_color,
    help_popup_heading_color, muted_text_color,
};
use crate::tui_reducer_state::{GrantDialogButton, GrantDialogLayout, GrantDialogState};
use crate::tui_render_state::TuiState;

use super::chat::render_chat_scrollbar;
use super::footer::button_style;

const MAXIMUM_DIALOG_WIDTH: u16 = 76;
const CONFIRM_LABEL: &str = " [ ✓ Grant ] ";
const BACK_LABEL: &str = " [ ← Back ] ";

/// Greedy word wrap of `text` to `width` columns; never returns no lines.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let word_width = word.chars().count();
        let current_width = current.chars().count();
        if !current.is_empty() && current_width + 1 + word_width > width {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
        while current.chars().count() > width {
            let rest: String = current.chars().skip(width).collect();
            lines.push(current.chars().take(width).collect());
            current = rest;
        }
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

fn indented(text: &str, first: &str, width: usize) -> Vec<String> {
    let rest = " ".repeat(first.chars().count());
    wrap(text, width.saturating_sub(first.chars().count()))
        .into_iter()
        .enumerate()
        .map(|(index, line)| format!("{}{line}", if index == 0 { first } else { &rest }))
        .collect()
}

fn heading(text: &str) -> Line<'static> {
    Line::styled(
        text.to_string(),
        Style::default()
            .fg(help_popup_heading_color())
            .add_modifier(Modifier::BOLD),
    )
}

fn muted(text: String) -> Line<'static> {
    Line::styled(text, Style::default().fg(muted_text_color()))
}

fn dialog_width(frame_width: u16) -> u16 {
    frame_width.saturating_sub(2).clamp(1, MAXIMUM_DIALOG_WIDTH)
}

/// The rule lines, the lifetime lines and the note lines of `dialog`.
fn build_sections(
    dialog: &GrantDialogState,
    text_width: usize,
) -> (Vec<Line<'static>>, Vec<Line<'static>>, Vec<Line<'static>>) {
    let plain = |text: String| Line::styled(text, Style::default().fg(content_color()));

    let mut rules = vec![heading(if dialog.preview.refusal.is_some() {
        "Cannot be granted:"
    } else {
        "Proposed rules:"
    })];
    match &dialog.preview.refusal {
        Some(reason) => rules.extend(wrap(reason, text_width).into_iter().map(plain)),
        None => {
            for rule in &dialog.preview.rules {
                rules.extend(indented(rule, "  ", text_width).into_iter().map(plain));
            }
        }
    }

    let radio = |lifetime: GrantLifetime, label: &str, available: bool| {
        let selected = dialog.lifetime == lifetime;
        let marker = if selected { "(•)" } else { "( )" };
        let line = format!(" {marker} {label}");
        if available {
            let style = if selected {
                Style::default()
                    .fg(content_color())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(content_color())
            };
            Line::styled(line, style)
        } else {
            muted(line)
        }
    };
    let persistent_available = dialog.preview.persistent_unavailable.is_none();
    let mut lifetime_lines = vec![
        heading("Lifetime (←/→ or t/p):"),
        radio(
            GrantLifetime::Temporary,
            "Temporary: until the conversation is cleared",
            true,
        ),
        radio(
            GrantLifetime::Persistent,
            "Persistent: saved to this profile's grants.toml",
            persistent_available,
        ),
    ];
    if let Some(reason) = &dialog.preview.persistent_unavailable {
        lifetime_lines.extend(
            indented(&format!("Unavailable: {reason}"), "     ", text_width)
                .into_iter()
                .map(muted),
        );
    }

    let mut notes: Vec<Line> = Vec::new();
    if dialog.preview.refusal.is_none() {
        for warning in &dialog.preview.warnings {
            notes.extend(indented(warning, " • ", text_width).into_iter().map(muted));
        }
    }
    (rules, lifetime_lines, notes)
}

/// Fewest terminal rows the dialog is drawn in: borders, a few body rows and
/// the button row. Taller content scrolls.
const MINIMUM_DIALOG_HEIGHT: u16 = 7;

pub(super) fn grant_dialog_required_height() -> u16 {
    MINIMUM_DIALOG_HEIGHT
}

/// Draws the dialog and records its geometry in `state.grant_dialog_layout`.
/// The body scrolls; the button row stays pinned below it.
pub(super) fn draw_grant_dialog(
    frame: &mut Frame,
    state: &mut TuiState,
    mouse_pos: Option<(u16, u16)>,
) {
    let Some(dialog) = state.grant_dialog.clone() else {
        return;
    };
    let area = frame.area();
    if grant_dialog_required_height() > area.height {
        state.grant_dialog_layout = GrantDialogLayout::default();
        return;
    }
    let width = dialog_width(area.width);
    // One column each side for padding; the right one carries the scrollbar.
    let text_width = usize::from(width.saturating_sub(4)).max(1);

    let (rules, lifetime_lines, notes) = build_sections(&dialog, text_width);
    let mut body: Vec<Line> = rules;
    body.push(Line::default());
    let lifetime_top = body.len();
    body.extend(lifetime_lines);
    if !notes.is_empty() {
        body.push(Line::default());
        body.push(heading("Notes:"));
        body.extend(notes);
    }
    let height = (body.len() as u16 + 3).min(area.height);
    let body_rows = usize::from(height - 3);
    let maximum_scroll = body.len().saturating_sub(body_rows);
    let scroll = dialog.scroll_offset.min(maximum_scroll);
    let rect = Rect::new(
        area.x + (area.width.saturating_sub(width)) / 2,
        area.y + (area.height.saturating_sub(height)) / 2,
        width.min(area.width),
        height,
    );

    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(help_popup_border_color()))
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

    let row_rect =
        |body_row: usize| match body_row.checked_sub(scroll).filter(|row| *row < body_rows) {
            Some(row) => Rect::new(body_area.x, body_area.y + row as u16, body_area.width, 1),
            None => Rect::default(),
        };
    let buttons = Rect::new(content.x, body_area.bottom(), content.width, 1);
    let confirm_width = (CONFIRM_LABEL.chars().count() as u16).min(buttons.width);
    let confirm_button = Rect::new(buttons.x, buttons.y, confirm_width, buttons.height.min(1));
    let back_start = (buttons.x + confirm_width + 2).min(buttons.right());
    let back_button = Rect::new(
        back_start,
        buttons.y,
        (BACK_LABEL.chars().count() as u16).min(buttons.right() - back_start),
        buttons.height.min(1),
    );
    draw_buttons(frame, mouse_pos, &dialog, confirm_button, back_button);

    state.grant_dialog_layout = GrantDialogLayout {
        rect,
        temporary_row: row_rect(lifetime_top + 1),
        persistent_row: row_rect(lifetime_top + 2),
        confirm_button,
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
    back: Rect,
) {
    let pointer = mouse_pos.map(|(column, row)| ratatui::layout::Position::new(column, row));
    let hovered = |area: Rect| pointer.is_some_and(|position| area.contains(position));
    let confirm_style = if dialog.can_confirm() {
        button_style(
            approve_background_color(),
            approve_hover_background_color(),
            hovered(confirm),
            dialog.focused == GrantDialogButton::Confirm,
        )
    } else {
        Style::default().fg(muted_text_color())
    };
    let back_style = button_style(
        deny_background_color(),
        deny_hover_background_color(),
        hovered(back),
        dialog.focused == GrantDialogButton::Back,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(CONFIRM_LABEL, confirm_style))),
        confirm,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(BACK_LABEL, back_style))),
        back,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use uuid::Uuid;

    fn dialog(
        warnings: usize,
        persistent_unavailable: Option<&str>,
        refusal: Option<&str>,
    ) -> GrantDialogState {
        GrantDialogState {
            preview: crate::agent::GrantPreview {
                id: Uuid::new_v4(),
                rules: vec!["bash 'make test'".to_string()],
                refusal: refusal.map(str::to_string),
                warnings: (0..warnings)
                    .map(|index| format!("Warning number {index} with some words to wrap"))
                    .collect(),
                persistent_unavailable: persistent_unavailable.map(str::to_string),
            },
            lifetime: GrantLifetime::Temporary,
            focused: GrantDialogButton::Confirm,
            scroll_offset: 0,
        }
    }

    fn render(width: u16, height: u16, dialog: GrantDialogState) -> (String, GrantDialogLayout) {
        let mut state = TuiState::new();
        state.grant_dialog = Some(dialog);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| draw_grant_dialog(frame, &mut state, None))
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
    fn the_dialog_shows_rules_lifetimes_notes_and_buttons_with_hit_areas() {
        let (text, layout) = render(80, 24, dialog(2, None, None));
        assert_eq!(layout.maximum_scroll, 0);
        for expected in [
            "bash 'make test'",
            "Temporary",
            "Persistent",
            "Notes:",
            "Grant ]",
            "Back ]",
        ] {
            assert!(text.contains(expected), "{expected}: {text}");
        }
        assert!(!layout.confirm_button.is_empty() && !layout.back_button.is_empty());
        assert!(layout.temporary_row.y < layout.persistent_row.y);
        assert!(layout.persistent_row.y < layout.confirm_button.y);
    }

    #[test]
    fn a_short_terminal_scrolls_the_body_and_keeps_the_buttons() {
        let mut long = dialog(6, Some("no profile directory"), None);
        let (text, layout) = render(40, 9, long.clone());
        assert!(!layout.confirm_button.is_empty(), "{text}");
        assert!(layout.maximum_scroll > 0, "{text}");
        assert!(!text.contains("Notes:"), "{text}");
        long.scroll_offset = usize::MAX;
        let (text, layout) = render(40, 9, long);
        assert!(text.contains("Warning number 5"), "{text}");
        assert!(!layout.confirm_button.is_empty(), "{text}");
        assert!(layout.temporary_row.is_empty());
        let (_, layout) = render(
            40,
            grant_dialog_required_height() - 1,
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

    #[test]
    fn wrap_breaks_at_words_and_splits_overlong_words() {
        assert_eq!(wrap("aa bb cc", 5), vec!["aa bb", "cc"]);
        assert_eq!(wrap("abcdefg", 3), vec!["abc", "def", "g"]);
        assert_eq!(wrap("", 3), vec![String::new()]);
    }
}
