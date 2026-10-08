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

//! Pure Presenter layer for the reducer pattern.
//!
//! [`update`] is the single entry point: it takes the current [`AppState`] by
//! mutable reference (so pure state mutations — scroll offsets, focus changes,
//! mode transitions — can happen in-place) and returns a `Vec<Effect>` for
//! every I/O action the Driver must execute.
//!
//! **No `use ratatui`**, no `use ratatui_textarea`**, no channel sends.

use crate::tui_reducer_state::{AppState, DomainEvent, Effect, InputEvent};

mod domain;
mod keyboard;
mod mouse;
mod tool_bubbles;

use domain::route_domain;
use keyboard::route_keyboard;
use mouse::route_mouse;

// ─── Public surface ───────────────────────────────────────────────────────────

/// Typed event surface accepted by [`update`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiEvent {
    Input(InputEvent),
    Mouse(crossterm::event::MouseEvent),
    Domain(DomainEvent),
}

/// Read-only snapshot of the textarea state that the Presenter needs for
/// routing decisions without importing ratatui-textarea.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextareaContext {
    /// `true` when the textarea contains only whitespace / is empty.
    pub is_empty: bool,
    /// Joined text of all textarea lines (used for submit / dedent).
    pub text: String,
    /// Zero-based cursor row inside the textarea.
    pub cursor_row: usize,
    /// `true` when the cursor is on the last row of the textarea.
    pub cursor_row_is_last: bool,
}

/// Apply one event to `state`, return the effects the Driver must execute.
pub fn update(state: &mut AppState, event: TuiEvent, textarea: &TextareaContext) -> Vec<Effect> {
    match event {
        TuiEvent::Input(input_event) => route_keyboard(state, input_event, textarea),
        TuiEvent::Mouse(mouse_event) => route_mouse(state, mouse_event, textarea),
        TuiEvent::Domain(domain_event) => route_domain(state, domain_event),
    }
}

#[cfg(test)]
mod tests;
