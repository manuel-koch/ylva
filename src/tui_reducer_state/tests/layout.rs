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

use super::*;
use ratatui::layout::Rect;

use crate::tui_reducer_state::PickerRowLayout;

#[test]
fn chat_area_contains_detects_interior_cells() {
    let area = ChatArea {
        rect: Rect::new(2, 3, 10, 5), // x=2, y=3, w=10, h=5
    };
    // Interior
    assert!(area.contains(2, 3)); // top-left corner
    assert!(area.contains(11, 7)); // bottom-right corner
    assert!(area.contains(5, 5)); // somewhere inside

    // Exterior
    assert!(!area.contains(1, 3)); // left of area
    assert!(!area.contains(12, 3)); // right of area (x + w = 12)
    assert!(!area.contains(2, 2)); // above area
    assert!(!area.contains(2, 8)); // below area (y + h = 8)
}

#[test]
fn chat_area_page_scroll_rows_returns_height_minus_one() {
    let area = ChatArea {
        rect: Rect::new(0, 0, 80, 20),
    };
    assert_eq!(area.page_scroll_rows(), 19);
}

#[test]
fn chat_area_page_scroll_rows_saturates_at_zero_for_tiny_area() {
    let area = ChatArea {
        rect: Rect::new(0, 0, 80, 0),
    };
    assert_eq!(area.page_scroll_rows(), 0);
}

#[test]
fn scrollbar_maps_arrows_and_track_to_scroll_offsets() {
    let scrollbar = ChatScrollbarLayout {
        rect: Rect::new(79, 3, 1, 7),
        maximum_scroll_offset: 100,
    };
    for (row, expected) in [
        (3, 0),
        (4, 0),
        (5, 25),
        (6, 50),
        (7, 75),
        (8, 100),
        (9, 100),
    ] {
        assert_eq!(scrollbar.offset_at(79, row), Some(expected));
    }
    for (column, row) in [(78, 3), (80, 3), (79, 2), (79, 10)] {
        assert_eq!(scrollbar.offset_at(column, row), None);
    }
    assert_eq!(ChatScrollbarLayout::default().offset_at(0, 0), None);
}

#[test]
fn scrollbar_handles_short_tracks_and_large_scroll_ranges() {
    for (height, expected_offsets) in [
        (1, vec![0]),
        (2, vec![0, usize::MAX]),
        (3, vec![0, usize::MAX / 2, usize::MAX]),
        (4, vec![0, 0, usize::MAX, usize::MAX]),
    ] {
        let scrollbar = ChatScrollbarLayout {
            rect: Rect::new(4, 5, 1, height),
            maximum_scroll_offset: usize::MAX,
        };
        for (row, expected) in expected_offsets.into_iter().enumerate() {
            assert_eq!(scrollbar.offset_at(4, 5 + row as u16), Some(expected));
        }
    }
}

#[test]
fn input_area_contains_detects_interior_cells() {
    let area = InputAreaLayout {
        rect: Rect::new(0, 20, 80, 4),
    };
    assert!(area.contains(0, 20));
    assert!(area.contains(79, 23));
    assert!(!area.contains(80, 20));
    assert!(!area.contains(0, 24));
}

#[test]
fn help_popup_hit_detects_interior_cells() {
    let popup = HelpPopupLayout {
        rect: Rect::new(10, 5, 60, 30),
        content_area: Rect::default(),
    };
    assert!(popup.hit(10, 5));
    assert!(popup.hit(69, 34));
    assert!(!popup.hit(9, 5));
    assert!(!popup.hit(70, 5));
}

#[test]
fn page_entry_count_is_the_number_of_whole_entries_that_fit() {
    let layout_with = |height: u16, row_layout: PickerRowLayout| SlashOverlayLayout {
        content_area: Rect::new(0, 0, 10, height),
        row_layout,
        ..SlashOverlayLayout::default()
    };

    assert_eq!(
        SlashOverlayLayout::default().page_entry_count(),
        3,
        "before the first draw there is no geometry, so the fallback step applies"
    );
    assert_eq!(
        layout_with(10, PickerRowLayout::SingleRow).page_entry_count(),
        10
    );
    assert_eq!(
        layout_with(10, PickerRowLayout::TwoRows).page_entry_count(),
        5,
        "two rows per model halve the entries a page holds"
    );
    assert_eq!(
        layout_with(1, PickerRowLayout::TwoRows).page_entry_count(),
        1,
        "a page always moves by at least one entry"
    );
}
