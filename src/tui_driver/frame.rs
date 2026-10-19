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

//! Frame drawing and the footer confirmation hint.

use ratatui::backend::Backend;
use ratatui::Terminal;
use ratatui_textarea::TextArea;

use crate::agent::AgentLoop;
use crate::presenter::{self, TextareaContext, TuiEvent};
use crate::render;
use crate::tui_reducer_state::{AppState, ConfirmKey, DomainEvent, TuiMode};

/// Draw one frame from the driver's current state.
///
/// Keeps the event-loop render path in one place, including the live engine
/// display and confirmation hint.
pub(super) fn draw_frame<B>(
    terminal: &mut Terminal<B>,
    state: &mut AppState,
    textarea: &mut TextArea<'_>,
    agent: &AgentLoop,
    mouse_pos: Option<(u16, u16)>,
) -> anyhow::Result<()>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    state.engine_display = agent.engine_display_info();
    let hint = confirm_hint(state);
    if state.pending_bubble_expansion.is_some() {
        terminal.draw(|frame| {
            render::render_expansion_feedback(
                frame,
                state,
                textarea,
                hint.as_deref(),
                mouse_pos,
                agent.registry().as_ref(),
            );
        })?;
        presenter::update(
            state,
            TuiEvent::Domain(DomainEvent::BubbleExpansionFeedbackShown),
            &TextareaContext {
                is_empty: textarea.lines() == [""],
                text: textarea.lines().join("\n"),
                cursor_row: textarea.cursor().0,
                cursor_row_is_last: textarea.cursor().0 + 1 >= textarea.lines().len().max(1),
            },
        );
    }
    terminal.draw(|frame| {
        render::render(
            frame,
            state,
            textarea,
            hint.as_deref(),
            mouse_pos,
            agent.registry().as_ref(),
        );
    })?;
    Ok(())
}

/// Returns the footer hint string when a double-press confirm is pending.
pub(super) fn confirm_hint(state: &AppState) -> Option<String> {
    if matches!(state.mode, TuiMode::Aborting) {
        return None;
    }
    if let TuiMode::DoublePressAbort {
        return_to,
        armed_by,
        ..
    } = &state.mode
    {
        let is_idle = matches!(**return_to, TuiMode::Idle);
        // Only the same key's second press confirms (see `ConfirmKey`), so
        // name that one key explicitly rather than both — naming both would
        // wrongly imply either key finishes the same action.
        let hint = match (armed_by, is_idle) {
            (ConfirmKey::CtrlC, true) => " Press ctrl-c again to quit",
            (ConfirmKey::CtrlC, false) => " Press ctrl-c again to cancel & quit",
            (ConfirmKey::Escape, true) => " Press esc again to clear",
            (ConfirmKey::Escape, false) => " Press esc again to cancel",
        };
        Some(hint.to_string())
    } else {
        None
    }
}
