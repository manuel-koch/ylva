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

//! Status popup (`/status`): the snapshot being shown and its scroll position.

use super::{AppState, ChatSelection, ConversationView, Focus};
use crate::agent::StatusSnapshot;

/// Snapshot and scroll state of the status popup.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusPopupState {
    pub snapshot: Option<StatusSnapshot>,
    pub scroll_offset: usize,
    /// Plain text of every content line as last drawn; selection coordinates
    /// index into it.
    pub rendered_lines: Vec<String>,
    pub selection: Option<ChatSelection>,
    pub drag_active: bool,
}

impl AppState {
    /// Shows `snapshot` in the status popup, replacing any previous one.
    pub fn open_status_popup(&mut self, snapshot: StatusSnapshot) {
        self.focus = Focus::StatusPopup;
        self.clear_help_selection();
        self.status_popup = StatusPopupState {
            snapshot: Some(snapshot),
            ..Default::default()
        };
    }

    /// Closes the popup, resuming an outstanding approval like the help popup.
    pub fn close_status_popup(&mut self) {
        self.restore_focus_after_help();
        self.status_popup = StatusPopupState::default();
    }

    /// Scrolls the popup content by `rows` (negative = up); the renderer
    /// clamps the offset to the content.
    pub fn status_popup_scroll(&mut self, rows: isize) {
        self.status_popup.scroll_offset =
            self.status_popup.scroll_offset.saturating_add_signed(rows);
    }

    /// Content position (absolute line, column) under a screen cell, or `None`
    /// outside the text area.
    pub fn status_popup_content_position(&self, column: u16, row: u16) -> Option<(usize, usize)> {
        let content_area = self.layout.status_popup.content_area;
        if !content_area.contains((column, row).into()) {
            return None;
        }
        Some((
            self.status_popup.scroll_offset + row.saturating_sub(content_area.y) as usize,
            column.saturating_sub(content_area.x) as usize,
        ))
    }

    /// Like [`Self::status_popup_content_position`], clamped into the text area
    /// so a drag may leave it.
    pub fn status_popup_drag_position(&self, column: u16, row: u16) -> Option<(usize, usize)> {
        let content_area = self.layout.status_popup.content_area;
        if content_area.width == 0 || content_area.height == 0 {
            return None;
        }
        let clamped_column = column.clamp(
            content_area.x,
            content_area.x + content_area.width.saturating_sub(1),
        );
        let clamped_row = row.clamp(
            content_area.y,
            content_area.y + content_area.height.saturating_sub(1),
        );
        Some((
            self.status_popup.scroll_offset + (clamped_row - content_area.y) as usize,
            (clamped_column - content_area.x) as usize,
        ))
    }

    /// The text covered by the current status-popup selection.
    pub fn extract_status_selection_text(&self) -> Option<String> {
        super::selection::extract_selection_text(
            self.status_popup.selection.as_ref(),
            &self.status_popup.rendered_lines,
            &[],
        )
    }

    /// The `run_id` of the subagent being viewed, or `None` for the root.
    pub fn viewed_subagent_run_id(&self) -> Option<uuid::Uuid> {
        match &self.active_view {
            ConversationView::Root => None,
            ConversationView::Subagent { run_id, .. } => Some(*run_id),
        }
    }

    pub fn show_status_popup(&self) -> bool {
        matches!(self.focus, Focus::StatusPopup)
    }
}
