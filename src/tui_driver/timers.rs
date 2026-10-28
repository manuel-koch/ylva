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

//! Spinner gating and the deferred-timeout futures of the event loop.

use std::time::Instant;

use crate::agent::AgentMode;
use crate::tui_reducer_state::{AppState, TuiMode, CONFIRM_TIMEOUT};

/// Whether the footer/header spinner glyph should advance this tick.
///
/// Gates on `agent_mode()`, not raw `TuiMode`, so the spinner keeps advancing
/// whenever *anything* is animating a spinner glyph — including a
/// `spawn_subagent` tool bubble (and its subagent-view header re-render) that's
/// still `Running` while the root's own `TuiMode` has already gone `Idle`
/// waiting on it, regardless of whether the subagent itself is currently
/// paused for its own tool approval (that only affects the subagent's
/// *inner* bubble, not the root `spawn_subagent` bubble's `Running` state).
/// `DoublePressAbort` stays frozen regardless, matching its static
/// confirm-prompt UI.
pub(super) fn spinner_should_tick(state: &AppState) -> bool {
    !matches!(state.mode, TuiMode::DoublePressAbort { .. }) && state.agent_mode() != AgentMode::Idle
}

/// A future that resolves once the double-press confirm window elapses (or
/// never, when no confirm is pending).
pub(super) async fn confirm_timeout(state: &AppState) {
    if let TuiMode::DoublePressAbort { pressed_at, .. } = &state.mode {
        let remaining = (*pressed_at + CONFIRM_TIMEOUT).saturating_duration_since(Instant::now());
        tokio::time::sleep(remaining).await;
    } else {
        std::future::pending::<()>().await;
    }
}

/// A future that resolves once a bubble's deferred single-click
/// collapse/expand toggle window elapses (or never, when no click is
/// currently armed). Armed by a first click on a `spawn_subagent` bubble's
/// title — see `crate::tui_reducer_state::DOUBLE_CLICK_WINDOW` for why the
/// toggle is deferred rather than applied immediately.
pub(super) async fn bubble_click_timeout(state: &AppState) {
    if let Some((_, clicked_at)) = state.last_bubble_click {
        let remaining = (clicked_at + crate::tui_reducer_state::DOUBLE_CLICK_WINDOW)
            .saturating_duration_since(Instant::now());
        tokio::time::sleep(remaining).await;
    } else {
        std::future::pending::<()>().await;
    }
}

/// A future that resolves once the current typing burst counts as over — the
/// idle delay after the last keystroke or the deferral cap, whichever comes
/// first (or never, when nobody is typing). See `AppState::note_keystroke`.
pub(super) async fn typing_settle_timeout(state: &AppState) {
    if let Some(settles_at) = state.typing_settles_at() {
        tokio::time::sleep(settles_at.saturating_duration_since(Instant::now())).await;
    } else {
        std::future::pending::<()>().await;
    }
}
