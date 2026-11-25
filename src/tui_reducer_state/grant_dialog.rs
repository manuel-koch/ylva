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

//! Grant dialog: the second page of the tool-approval prompt, where the user
//! reviews the proposed grants, selects and edits them per row and confirms or
//! goes back.

use super::{AppState, DoubleClickTracker, Effect, Focus, NameCursorMove};
use crate::agent::GrantPreview;
use crate::grants::{grant_warnings, validate_draft, Grant, GrantDraft, GrantRequest, GrantScope};

/// What a dialog row creates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantRowSelection {
    OneTime,
    Temporary,
    Persistent,
}

impl GrantRowSelection {
    pub fn label(self) -> &'static str {
        match self {
            GrantRowSelection::OneTime => "one-time",
            GrantRowSelection::Temporary => "temporary",
            GrantRowSelection::Persistent => "persistent",
        }
    }

    fn scope(self) -> Option<GrantScope> {
        match self {
            GrantRowSelection::OneTime => None,
            GrantRowSelection::Temporary => Some(GrantScope::Temporary),
            GrantRowSelection::Persistent => Some(GrantScope::Persistent),
        }
    }
}

/// One proposed grant of the dialog, possibly edited by the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantDialogRow {
    pub draft: GrantDraft,
    pub selection: GrantRowSelection,
}

/// Which element of the grant dialog has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantDialogFocus {
    Row(usize),
    Confirm,
    Back,
}

/// An edit of a row's rule in progress; `original` is restored on cancel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantRowEdit {
    pub row: usize,
    pub original: Option<String>,
    /// Caret position as a character index into the rule.
    pub cursor: usize,
}

/// Why the last confirmation created nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantFailure {
    pub reason: String,
    pub reloadable: bool,
}

/// The open grant dialog for the pending approval `preview.id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantDialogState {
    pub preview: GrantPreview,
    pub rows: Vec<GrantDialogRow>,
    pub focused: GrantDialogFocus,
    pub editing: Option<GrantRowEdit>,
    pub failure: Option<GrantFailure>,
    /// A confirmation was sent and the agent has not answered yet.
    pub awaiting_answer: bool,
    /// First visible body row; the renderer clamps it to the content.
    pub scroll_offset: usize,
    /// The row whose rule was clicked last, to recognise a double-click.
    pub rule_click: DoubleClickTracker<usize>,
}

fn character_byte_index(text: &str, character_index: usize) -> usize {
    text.char_indices()
        .nth(character_index)
        .map_or(text.len(), |(index, _)| index)
}

impl GrantDialogState {
    fn persistent_available(&self) -> bool {
        self.preview.persistent_unavailable.is_none()
    }

    /// Why the rule of row `index` cannot be created, if it cannot.
    pub fn row_error(&self, index: usize) -> Option<String> {
        let row = self.rows.get(index)?;
        validate_draft(
            &row.draft,
            &self.preview.subjects,
            &self.preview.workspace_root,
        )
        .err()
        .map(|error| error.0)
    }

    fn selected_rows(&self) -> impl Iterator<Item = &GrantDialogRow> {
        self.rows
            .iter()
            .filter(|row| row.selection != GrantRowSelection::OneTime)
    }

    /// Whether the selection can be confirmed: at least one row that is
    /// not `one-time`, every such row valid, nothing being edited or awaiting an answer.
    pub fn can_confirm(&self) -> bool {
        self.preview.refusal.is_none()
            && self.editing.is_none()
            && !self.awaiting_answer
            && self.selected_rows().next().is_some()
            && self.rows.iter().enumerate().all(|(index, row)| {
                row.selection == GrantRowSelection::OneTime || self.row_error(index).is_none()
            })
    }

    /// Consequences of the selected rows (all valid rows while none is
    /// selected), for the notes of the dialog.
    pub fn warnings(&self) -> Vec<String> {
        let selected: Vec<&GrantDialogRow> = self.selected_rows().collect();
        let considered: Vec<&GrantDialogRow> = if selected.is_empty() {
            self.rows.iter().collect()
        } else {
            selected
        };
        let grants: Vec<Grant> = considered
            .iter()
            .filter_map(|row| {
                validate_draft(
                    &row.draft,
                    &self.preview.subjects,
                    &self.preview.workspace_root,
                )
                .ok()
            })
            .collect();
        let persistent = considered
            .iter()
            .any(|row| row.selection == GrantRowSelection::Persistent);
        grant_warnings(
            &grants,
            persistent,
            self.preview.marker_to_create.as_deref(),
        )
    }

    fn requests(&self) -> Vec<GrantRequest> {
        self.rows
            .iter()
            .filter_map(|row| {
                Some(GrantRequest {
                    draft: row.draft.clone(),
                    scope: row.selection.scope()?,
                })
            })
            .collect()
    }
}

impl AppState {
    /// Opens the dialog over the approval prompt it was requested for. Ignored
    /// when that prompt is no longer the pending approval.
    pub fn open_grant_dialog(&mut self, preview: GrantPreview) {
        let pending =
            matches!(&self.focus, Focus::ToolApproval { tool_id, .. } if *tool_id == preview.id);
        if !pending {
            return;
        }
        let rows = preview
            .rows
            .iter()
            .map(|draft| GrantDialogRow {
                draft: draft.clone(),
                selection: GrantRowSelection::Temporary,
            })
            .collect();
        self.grant_dialog = Some(GrantDialogState {
            preview,
            rows,
            focused: GrantDialogFocus::Confirm,
            editing: None,
            failure: None,
            awaiting_answer: false,
            scroll_offset: 0,
            rule_click: DoubleClickTracker::default(),
        });
    }

    /// Returns to the approval prompt.
    pub fn close_grant_dialog(&mut self) {
        self.grant_dialog = None;
    }

    /// The dialog, only while its approval is still the pending one.
    pub fn visible_grant_dialog(&self) -> Option<&GrantDialogState> {
        let dialog = self.grant_dialog.as_ref()?;
        match &self.focus {
            Focus::ToolApproval { tool_id, .. } if *tool_id == dialog.preview.id => Some(dialog),
            _ => None,
        }
    }

    /// The agent created nothing for approval `id`: show why and allow a
    /// retry, a switch to temporary grants, or going back.
    pub fn grant_dialog_failed(&mut self, id: uuid::Uuid, reason: String, reloadable: bool) {
        if let Some(dialog) = self
            .grant_dialog
            .as_mut()
            .filter(|dialog| dialog.preview.id == id)
        {
            dialog.awaiting_answer = false;
            dialog.failure = Some(GrantFailure { reason, reloadable });
            dialog.focused = GrantDialogFocus::Confirm;
        }
    }

    /// Sets the selection of row `index`; persistent is refused while saving
    /// is unavailable.
    pub fn select_grant_row(&mut self, index: usize, selection: GrantRowSelection) {
        if let Some(dialog) = self.grant_dialog.as_mut() {
            if selection == GrantRowSelection::Persistent && !dialog.persistent_available() {
                return;
            }
            if let Some(row) = dialog.rows.get_mut(index) {
                row.selection = selection;
            }
        }
    }

    /// Cycles row `index` through off, temporary and persistent (when
    /// available).
    pub fn cycle_grant_row(&mut self, index: usize, forward: bool) {
        let Some(dialog) = self.grant_dialog.as_ref() else {
            return;
        };
        let Some(row) = dialog.rows.get(index) else {
            return;
        };
        let mut order = vec![GrantRowSelection::OneTime, GrantRowSelection::Temporary];
        if dialog.persistent_available() {
            order.push(GrantRowSelection::Persistent);
        }
        let position = order
            .iter()
            .position(|candidate| *candidate == row.selection)
            .unwrap_or(0);
        let next = if forward {
            (position + 1) % order.len()
        } else {
            (position + order.len() - 1) % order.len()
        };
        self.select_grant_row(index, order[next]);
    }

    /// Cycles the selection of the focused row.
    pub fn cycle_grant_focused(&mut self, forward: bool) {
        if let Some(GrantDialogFocus::Row(index)) =
            self.grant_dialog.as_ref().map(|dialog| dialog.focused)
        {
            self.cycle_grant_row(index, forward);
        }
    }

    /// Turns the focused row off, or on as a temporary grant.
    pub fn toggle_grant_focused(&mut self) {
        if let Some(GrantDialogFocus::Row(index)) =
            self.grant_dialog.as_ref().map(|dialog| dialog.focused)
        {
            let on = self
                .grant_dialog
                .as_ref()
                .and_then(|dialog| dialog.rows.get(index))
                .is_some_and(|row| row.selection == GrantRowSelection::OneTime);
            self.select_grant_row(
                index,
                if on {
                    GrantRowSelection::Temporary
                } else {
                    GrantRowSelection::OneTime
                },
            );
        }
    }

    /// Scrolls the body by `rows` (negative = up), within the content drawn last.
    pub fn scroll_grant_dialog(&mut self, rows: isize) {
        let maximum = self.layout.grant_dialog.maximum_scroll;
        if let Some(dialog) = self.grant_dialog.as_mut() {
            dialog.scroll_offset = dialog
                .scroll_offset
                .min(maximum)
                .saturating_add_signed(rows)
                .min(maximum);
        }
    }

    /// Moves the focus over the rows only (↑/↓); a button focus returns to the
    /// last row on ↑ and ignores ↓. The focus does not wrap.
    pub fn move_grant_row_focus(&mut self, backward: bool) {
        if let Some(dialog) = self.grant_dialog.as_mut() {
            if dialog.editing.is_some() || dialog.rows.is_empty() {
                return;
            }
            dialog.focused = match (dialog.focused, backward) {
                (GrantDialogFocus::Row(index), true) => {
                    GrantDialogFocus::Row(index.saturating_sub(1))
                }
                (GrantDialogFocus::Row(index), false) => {
                    GrantDialogFocus::Row((index + 1).min(dialog.rows.len() - 1))
                }
                (_, true) => GrantDialogFocus::Row(dialog.rows.len() - 1),
                (focused, false) => focused,
            };
        }
    }

    /// Cycles the focus over the first row and the buttons (Tab), like the
    /// parameters page of the model picker.
    pub fn cycle_grant_focus(&mut self, backward: bool) {
        if let Some(dialog) = self.grant_dialog.as_mut() {
            if dialog.editing.is_some() {
                return;
            }
            let stops = [
                GrantDialogFocus::Row(0),
                GrantDialogFocus::Confirm,
                GrantDialogFocus::Back,
            ];
            let current = match dialog.focused {
                GrantDialogFocus::Row(_) => 0,
                GrantDialogFocus::Confirm => 1,
                GrantDialogFocus::Back => 2,
            };
            let next = if backward {
                (current + stops.len() - 1) % stops.len()
            } else {
                (current + 1) % stops.len()
            };
            dialog.focused = stops[next];
        }
    }

    /// A click on the rule of row `index` focuses the row; a double-click
    /// starts editing the rule.
    pub fn click_grant_rule(&mut self, index: usize) {
        let editing_here = self
            .grant_dialog
            .as_ref()
            .and_then(|dialog| dialog.editing.as_ref())
            .is_some_and(|edit| edit.row == index);
        if editing_here {
            return;
        }
        self.end_grant_edit(true);
        self.focus_grant_row(index);
        let double_click = self
            .grant_dialog
            .as_mut()
            .is_some_and(|dialog| dialog.rule_click.register(index, std::time::Instant::now()));
        if double_click {
            self.begin_grant_edit();
        }
    }

    pub fn focus_grant_row(&mut self, index: usize) {
        if let Some(dialog) = self.grant_dialog.as_mut() {
            if index < dialog.rows.len() && dialog.editing.is_none() {
                dialog.focused = GrantDialogFocus::Row(index);
            }
        }
    }

    /// Starts editing the rule of the focused row; rows without a rule
    /// (`web_search`, `spawn_subagent`) are not editable.
    pub fn begin_grant_edit(&mut self) {
        if let Some(dialog) = self.grant_dialog.as_mut() {
            if let GrantDialogFocus::Row(index) = dialog.focused {
                if dialog.editing.is_none() {
                    if let Some(original) = dialog.rows[index].draft.rule.clone() {
                        dialog.editing = Some(GrantRowEdit {
                            row: index,
                            cursor: original.chars().count(),
                            original: Some(original),
                        });
                    }
                }
            }
        }
    }

    /// Inserts typed text at the caret of the rule being edited. Control
    /// characters (including newlines) are dropped: a rule is a single line.
    pub fn type_grant_edit(&mut self, text: &str) {
        self.with_grant_edit(|rule, cursor| {
            let inserted: String = text
                .chars()
                .filter(|character| !character.is_control())
                .collect();
            let byte_index = character_byte_index(rule, *cursor);
            rule.insert_str(byte_index, &inserted);
            *cursor += inserted.chars().count();
        });
    }

    /// Deletes the character before the caret, or all of the rule.
    pub fn delete_grant_edit(&mut self, everything: bool) {
        self.with_grant_edit(|rule, cursor| {
            if everything {
                rule.clear();
                *cursor = 0;
            } else if *cursor > 0 {
                *cursor -= 1;
                rule.remove(character_byte_index(rule, *cursor));
            }
        });
    }

    /// Deletes the character under the caret.
    pub fn delete_grant_edit_forward(&mut self) {
        self.with_grant_edit(|rule, cursor| {
            if *cursor < rule.chars().count() {
                rule.remove(character_byte_index(rule, *cursor));
            }
        });
    }

    /// Moves the caret of the rule being edited.
    pub fn move_grant_edit_cursor(&mut self, target: NameCursorMove) {
        self.with_grant_edit(|rule, cursor| {
            let length = rule.chars().count();
            *cursor = match target {
                NameCursorMove::Left => cursor.saturating_sub(1),
                NameCursorMove::Right => (*cursor + 1).min(length),
                NameCursorMove::Start => 0,
                NameCursorMove::End => length,
            };
        });
    }

    fn with_grant_edit(&mut self, change: impl FnOnce(&mut String, &mut usize)) {
        let Some(dialog) = self.grant_dialog.as_mut() else {
            return;
        };
        let Some(edit) = dialog.editing.as_mut() else {
            return;
        };
        if let Some(rule) = dialog.rows[edit.row].draft.rule.as_mut() {
            edit.cursor = edit.cursor.min(rule.chars().count());
            change(rule, &mut edit.cursor);
        }
    }

    /// Ends the edit, keeping the typed rule or restoring the original.
    pub fn end_grant_edit(&mut self, keep: bool) {
        if let Some(dialog) = self.grant_dialog.as_mut() {
            if let Some(edit) = dialog.editing.take() {
                if !keep {
                    dialog.rows[edit.row].draft.rule = edit.original;
                }
            }
        }
    }

    /// Confirms the selected rows. The dialog stays open until the agent
    /// approves the call (the prompt then goes away) or reports why nothing
    /// was created. Does nothing when the selection cannot be confirmed.
    /// `reload_snapshot` accepts a reviewed external edit of `grants.toml`.
    pub fn confirm_grant_dialog(&mut self, reload_snapshot: bool) -> Vec<Effect> {
        if !self
            .visible_grant_dialog()
            .is_some_and(GrantDialogState::can_confirm)
        {
            return vec![];
        }
        let Some(dialog) = self.grant_dialog.as_mut() else {
            return vec![];
        };
        dialog.awaiting_answer = true;
        dialog.failure = None;
        vec![Effect::GrantTool {
            id: dialog.preview.id,
            requests: dialog.requests(),
            reload_snapshot,
        }]
    }

    /// Retries after an external edit of `grants.toml` was reported.
    pub fn retry_grant_with_reload(&mut self) -> Vec<Effect> {
        let reloadable = self
            .grant_dialog
            .as_ref()
            .and_then(|dialog| dialog.failure.as_ref())
            .is_some_and(|failure| failure.reloadable);
        if reloadable {
            self.confirm_grant_dialog(true)
        } else {
            vec![]
        }
    }

    /// Activates the keyboard-focused element.
    pub fn activate_grant_dialog_focus(&mut self) -> Vec<Effect> {
        match self.grant_dialog.as_ref().map(|dialog| dialog.focused) {
            Some(GrantDialogFocus::Confirm) => self.confirm_grant_dialog(false),
            Some(GrantDialogFocus::Back) => {
                self.close_grant_dialog();
                vec![]
            }
            Some(GrantDialogFocus::Row(_)) => {
                self.begin_grant_edit();
                vec![]
            }
            None => vec![],
        }
    }
}
