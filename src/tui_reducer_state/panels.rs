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

use std::path::PathBuf;

use super::ChatSelection;

// ─── Per-area sub-structs ──────────────────────────────────────────────────────
//
// Groups related `AppState` fields by the user-facing area they belong to
// (help popup, drill-in subagent view, clarify banner, conversation list),
// mirroring the `AppLayout` precedent above. On-screen rects for these same
// areas stay in `AppLayout` (written once per frame by the Driver) rather
// than living here — these sub-structs are data-only. `TuiState`
// (`src/tui_render_state.rs`) defines its own, differently-shaped sub-structs
// with the same names: it has no separate layout type, so its versions carry
// their area's rect field(s) alongside the data. The two are intentionally
// distinct types, analogous per area rather than identical.

/// Help popup (`?` overlay) scroll state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HelpPopupState {
    pub help_scroll_offset: usize,
    pub help_scroll_x: usize,
    pub help_rendered_lines: Vec<String>,
    pub help_selection: Option<ChatSelection>,
    pub help_drag_active: bool,
}

/// Clarify-tool banner scroll/drag state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClarifyBannerState {
    /// Current vertical scroll offset inside the clarify banner (lines from
    /// the top of the content). Clamped to the valid range by
    /// `draw_clarify_banner` each frame.
    pub clarify_scroll_offset: usize,
    /// Terminal position where the left button was pressed inside a clarify
    /// choice row. Cleared on mouse-up. A mouse-up at the same position is a
    /// click (→ fill textarea); a mouse-up at a different position is a drag
    /// (→ no action, allows text selection).
    pub clarify_choice_mousedown: Option<(u16, u16)>,
    /// Updated on every `MouseDrag` event while `clarify_choice_mousedown` is
    /// set. Together with the down position this defines the highlighted row
    /// range in the banner. Cleared on mouse-up. Combined with
    /// `clarify_choice_mousedown` into a single `BannerDragSelection` by
    /// `render::project_for_draw` for `TuiState::clarify_banner_state`.
    pub clarify_banner_drag_end: Option<(u16, u16)>,
}

/// Active profile and workspace paths displayed in the TUI's startup logo
/// and help popup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TuiDisplayMetadata {
    pub profile_name: String,
    pub profile_path: Option<PathBuf>,
    pub ylva_home: Option<PathBuf>,
    pub workspace_root: Option<PathBuf>,
}

impl Default for TuiDisplayMetadata {
    fn default() -> Self {
        Self {
            profile_name: crate::config::DEFAULT_PROFILE_NAME.to_string(),
            profile_path: None,
            ylva_home: None,
            workspace_root: None,
        }
    }
}
