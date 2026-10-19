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

//! Pure TUI state model for the reducer pattern.
//!
//! This module defines the types consumed by `presenter::update()`:
//! - [`AppState`] — the authoritative view-model.
//! - [`TuiMode`] — agent-side control flow state.
//! - [`Focus`] — keyboard/mouse routing.
//! - [`InputEvent`] / [`DomainEvent`] — the typed event surface of the Presenter.
//! - [`Effect`] — the typed I/O commands the Driver executes.
//! - [`AppLayout`] — screen-space geometry with hit-test helpers.
//!
//! **No `use ratatui::backend`**, no channel types, no `UiUpdate` imports.
//! Only [`ratatui::layout::Rect`] is allowed from ratatui here.

use std::time::Duration;

pub use crate::agent::ContextUsageSource;

mod app_state;
mod context_usage;
mod conversation;
mod events;
mod input_history;
mod layout;
mod model_picker;
mod panels;
mod selection;
mod session_picker;

pub use app_state::AppState;
pub use context_usage::{format_context_usage, format_context_usage_readout, ContextUsage};
pub use conversation::{
    project_messages, result_section_ids, ActiveSubagentCounts, BubbleExpansionTarget, ChatEntry,
    ConversationListState, ConversationView, ResultSection, SubagentConversation, SubagentRunInfo,
    SubagentViewState, ToolCallState, TurnBuffer, UserMessageState,
};
pub use events::{ConfirmKey, DomainEvent, Effect, Focus, InputEvent, TuiMode};
pub use input_history::HistoryDirection;
pub use layout::{
    AppLayout, ChatArea, ChatScrollbarLayout, HelpPopupLayout, InputArea, InputAreaLayout,
    SlashOverlayLayout,
};
pub(crate) use model_picker::{
    engine_choice_parts, extract_models_filter, matching_model_indices, model_choice_parts,
    model_picker_row_entries, model_picker_row_is_details, model_picker_row_layout,
    slash_overlay_text_rows, visible_model_choices,
};
pub use model_picker::{
    ChoiceLabel, InputSnapshot, ModelChoice, ModelParameterEdit, ParameterField, PickerRowLayout,
    SlashOverlay,
};
pub use panels::{ClarifyBannerState, HelpPopupState, TuiDisplayMetadata};
pub use selection::{BannerDragSelection, ChatSelection};
pub use session_picker::{SessionAction, SessionChoice, SessionPicker, SessionPickerPhase};

// ─── Shared TUI constants ────────────────────────────────────────────────────

/// Duration a toast notification remains visible before it auto-dismisses.
const TOAST_DURATION: Duration = Duration::from_millis(1500);

/// Maximum gap between two clicks on the same chat bubble's title row for
/// the second click to count as a double-click (the drill-into-subagent
/// gesture) rather than a second, independent single click (collapse toggle).
pub(crate) const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(250);

/// Window for confirming the second press of a `Ctrl+C`/`Esc` action.
pub(crate) const CONFIRM_TIMEOUT: Duration = Duration::from_secs(2);

/// Number of columns moved by one horizontal help-popup scroll action.
pub(crate) const HELP_HORIZONTAL_SCROLL_COLUMNS: usize = 4;

/// Sentinel `entry_index` used in `thinking_title_rows` for the in-flight
/// (not yet flushed) thinking bubble rendered from `partial_thinking_text`.
/// Real entries are keyed by their `chat_entries` index (always `< usize::MAX`
/// in practice), so this value can never collide with a genuine index; it
/// lets click handling route to `AppState::toggle_partial_thinking_collapsed`
/// instead of `AppState::toggle_thinking_collapsed`.
pub const STREAMING_THINKING_ENTRY_INDEX: usize = usize::MAX;

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
