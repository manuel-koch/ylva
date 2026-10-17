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

//! Layout state and operations.

use ratatui::layout::Rect;

use super::PickerRowLayout;

/// The on-screen rect of the input box, written by `tui_widgets::draw` every frame and
/// read by the event loop for mouse click/drag cursor placement. Ephemeral
/// view state, reset to an empty rect between frames.
pub type InputArea = ratatui::layout::Rect;

/// Screen-space geometry for the chat scroll area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChatArea {
    pub rect: Rect,
}

impl ChatArea {
    /// Returns `true` if the terminal cell `(col, row)` falls inside this area.
    pub fn contains(&self, col: u16, row: u16) -> bool {
        col >= self.rect.x
            && col < self.rect.x + self.rect.width
            && row >= self.rect.y
            && row < self.rect.y + self.rect.height
    }

    /// Number of rows a page-up / page-down should scroll.
    pub fn page_scroll_rows(&self) -> u16 {
        self.rect.height.saturating_sub(1)
    }
}

/// Screen-space geometry for the text-input box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InputAreaLayout {
    pub rect: Rect,
}

impl InputAreaLayout {
    /// Returns `true` if the terminal cell `(col, row)` falls inside this area.
    pub fn contains(&self, col: u16, row: u16) -> bool {
        col >= self.rect.x
            && col < self.rect.x + self.rect.width
            && row >= self.rect.y
            && row < self.rect.y + self.rect.height
    }

    /// Number of rows a page-up / page-down should scroll.
    pub fn page_scroll_rows(&self) -> u16 {
        self.rect.height.saturating_sub(1)
    }
}

/// Screen-space geometry for the help popup overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HelpPopupLayout {
    pub rect: Rect,
    pub content_area: Rect,
}

impl HelpPopupLayout {
    /// Returns `true` if the terminal cell `(col, row)` falls inside the
    /// popup's rendered area (equivalent to `contains` but named `hit` to
    /// signal it is used for modal dismiss-on-outside-click logic).
    pub fn hit(&self, col: u16, row: u16) -> bool {
        col >= self.rect.x
            && col < self.rect.x + self.rect.width
            && row >= self.rect.y
            && row < self.rect.y + self.rect.height
    }

    /// Number of rows a page-up / page-down should scroll.
    pub fn page_scroll_rows(&self) -> u16 {
        self.rect.height.saturating_sub(1)
    }
}

/// Screen-space geometry for the slash-command overlay's selectable content.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SlashOverlayLayout {
    pub content_area: Rect,
    pub resume_action: Rect,
    pub clone_action: Rect,
    /// Absolute row in the overlay's content list corresponding to the first
    /// row currently rendered in `content_area`.
    pub first_visible_row: usize,
    /// Row layout the overlay was last drawn with. Input handling (click
    /// row mapping) and drag-copy must map rows the same way the renderer did.
    pub row_layout: PickerRowLayout,
    /// Screen rect of the footer text lines (below the separator), such as
    /// the `/models` compaction preview. Empty when the overlay has none.
    pub detail_area: Rect,
    /// Plain text of each footer line, as displayed, one entry per row of
    /// `detail_area` starting at its top.
    pub detail_rows: Vec<String>,
}

impl SlashOverlayLayout {
    /// Entries one `PageUp`/`PageDown` moves by: the whole entries that fit in
    /// the last drawn list. Before the first draw there is no geometry, so a
    /// small fixed step is used.
    pub fn page_entry_count(&self) -> isize {
        const FALLBACK_PAGE_ENTRY_COUNT: isize = 3;
        let visible_rows = self.content_area.height as usize;
        if visible_rows == 0 {
            return FALLBACK_PAGE_ENTRY_COUNT;
        }
        (visible_rows / self.row_layout.rows_per_entry()).max(1) as isize
    }

    /// Return the absolute content row under a terminal cell, if it is inside
    /// the rendered entry column.
    pub fn row_at(&self, col: u16, row: u16) -> Option<usize> {
        let area = self.content_area;
        if col < area.x || col >= area.x + area.width || row < area.y || row >= area.y + area.height
        {
            return None;
        }
        Some(self.first_visible_row + row.saturating_sub(area.y) as usize)
    }
}

/// Rendered conversation scrollbar geometry and its current scroll range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChatScrollbarLayout {
    /// Empty when the conversation does not overflow its viewport.
    pub rect: Rect,
    pub maximum_scroll_offset: usize,
}

impl ChatScrollbarLayout {
    pub fn offset_at(&self, column: u16, row: u16) -> Option<usize> {
        if !self.rect.contains((column, row).into()) {
            return None;
        }
        let height = usize::from(self.rect.height);
        let relative_row = usize::from(row - self.rect.y);
        let (track_row, track_height) = if height >= 4 {
            (relative_row.saturating_sub(1).min(height - 3), height - 2)
        } else {
            (relative_row, height)
        };
        let offset = if track_height <= 1 {
            0
        } else {
            (self.maximum_scroll_offset as u128 * track_row as u128 / (track_height - 1) as u128)
                as usize
        };
        Some(offset)
    }
}

/// Aggregated screen-space geometry written by the View each frame.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AppLayout {
    pub chat_area: ChatArea,
    pub chat_scrollbar: ChatScrollbarLayout,
    pub input_area: InputAreaLayout,
    pub help_popup: HelpPopupLayout,
    pub slash_overlay: SlashOverlayLayout,
    /// Rect of the clarify banner. Zero-sized when no clarify is pending.
    pub clarify_banner: ratatui::layout::Rect,
    /// Rect of the subagent-view header bubble's own content column (excludes
    /// its scrollbar column). Zero-sized while viewing `Root`. Used to route
    /// mouse-wheel scroll events to the header bubble rather than the chat
    /// area when the pointer is hovering over it.
    pub header_bubble: ratatui::layout::Rect,
    /// Rect of the Approve button rendered in the tool-approval footer.
    /// Written by the Driver after each `tui_widgets::draw` call; zero-sized when
    /// no approval footer is visible.
    pub approve_button: Rect,
    /// Rect of the Deny button rendered in the tool-approval footer.
    /// Written by the Driver after each `tui_widgets::draw` call; zero-sized when
    /// no approval footer is visible.
    pub deny_button: Rect,
}
