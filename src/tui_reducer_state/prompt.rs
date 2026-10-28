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

//! Prompt interaction state: typing-based deferral of approval/clarify
//! prompts and the focusable prompt buttons.
//!
//! Time is injected (`now` parameters) so the reducer never reads the clock.

use std::time::{Duration, Instant};

use super::{AppState, Focus, SlashOverlay, TuiMode};

/// Idle time after the last keystroke before a deferred prompt is shown.
pub const PROMPT_IDLE_DELAY: Duration = Duration::from_millis(1500);

/// Longest a prompt is deferred by continuous typing.
pub const PROMPT_DEFERRAL_CAP: Duration = Duration::from_secs(10);

/// A button of the tool-approval footer or the clarify banner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptButton {
    Approve,
    Deny,
    ClarifyOk,
    ClarifyCancel,
}

/// Keystroke timing used to defer prompts while the user is typing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InputActivity {
    last_keystroke_at: Option<Instant>,
    burst_started_at: Option<Instant>,
    /// `true` between a keystroke and the moment the idle delay or the cap
    /// elapsed (`AppState::settle_typing`).
    typing_in_progress: bool,
}

impl AppState {
    /// Records a keystroke at `now`. A keystroke after an idle gap starts a
    /// new typing burst, which restarts the deferral cap.
    pub fn note_keystroke(&mut self, now: Instant) {
        let activity = &mut self.input_activity;
        let burst_continues = activity.typing_in_progress
            && activity
                .last_keystroke_at
                .is_some_and(|last| now.saturating_duration_since(last) < PROMPT_IDLE_DELAY);
        if !burst_continues {
            activity.burst_started_at = Some(now);
        }
        activity.last_keystroke_at = Some(now);
        activity.typing_in_progress = true;
    }

    /// Whether a prompt arriving now has to wait for the typing to end.
    pub fn is_typing(&self) -> bool {
        self.input_activity.typing_in_progress
    }

    /// The instant at which the current typing burst counts as over: the
    /// idle delay after the last keystroke, but never later than the cap
    /// after the burst started. `None` when nobody is typing.
    pub fn typing_settles_at(&self) -> Option<Instant> {
        let activity = &self.input_activity;
        if !activity.typing_in_progress {
            return None;
        }
        let idle_deadline = activity.last_keystroke_at? + PROMPT_IDLE_DELAY;
        let cap_deadline = activity.burst_started_at? + PROMPT_DEFERRAL_CAP;
        Some(idle_deadline.min(cap_deadline))
    }

    /// Ends the typing burst; the caller then shows any deferred prompt.
    pub(crate) fn settle_typing(&mut self) {
        self.input_activity.typing_in_progress = false;
    }

    /// Number of approval/clarify prompts queued but not yet shown because
    /// the user is typing.
    pub fn deferred_prompt_count(&self) -> usize {
        if !self.is_typing() || self.prompt_is_visible() {
            return 0;
        }
        self.pending_approval_order
            .iter()
            .filter(|id| {
                self.queued_clarifies.contains_key(id) || self.pending_approval_name(**id).is_some()
            })
            .count()
    }

    /// Whether an approval footer or clarify banner currently owns the
    /// prompt area.
    pub fn prompt_is_visible(&self) -> bool {
        matches!(self.focus, Focus::ToolApproval { .. })
            || matches!(self.mode, TuiMode::ClarifyPending { .. })
    }

    /// Moves button focus to the next (or previous) button of the visible
    /// prompt: none → first → second → first …. A no-op without a prompt.
    pub(crate) fn cycle_prompt_button(&mut self, backwards: bool) {
        let (first, second) = if matches!(self.focus, Focus::ToolApproval { .. }) {
            (PromptButton::Approve, PromptButton::Deny)
        } else if matches!(self.mode, TuiMode::ClarifyPending { .. }) {
            (PromptButton::ClarifyOk, PromptButton::ClarifyCancel)
        } else {
            return;
        };
        let current = self
            .prompt_button_focus
            .filter(|button| *button == first || *button == second);
        self.prompt_button_focus = Some(match current {
            None if backwards => second,
            None => first,
            Some(button) if button == first => second,
            Some(_) => first,
        });
    }

    /// The focused button, when it belongs to the prompt currently shown.
    pub fn focused_prompt_button(&self) -> Option<PromptButton> {
        let button = self.prompt_button_focus?;
        let approval = matches!(self.focus, Focus::ToolApproval { .. });
        let clarify = matches!(self.mode, TuiMode::ClarifyPending { .. });
        match button {
            PromptButton::Approve | PromptButton::Deny if approval => Some(button),
            PromptButton::ClarifyOk | PromptButton::ClarifyCancel if clarify && !approval => {
                Some(button)
            }
            _ => None,
        }
    }

    /// Hides the slash overlay while a prompt owns the rows above the input,
    /// remembering it for `restore_slash_overlay`. The draft itself stays in
    /// the input field.
    pub(crate) fn park_slash_overlay(&mut self) {
        let overlay = std::mem::replace(&mut self.slash_overlay, SlashOverlay::Hidden);
        self.parked_slash_overlay.get_or_insert(overlay);
    }

    /// Gives back the overlay parked by `park_slash_overlay` once no prompt
    /// is visible any more.
    pub(crate) fn restore_slash_overlay(&mut self) {
        if self.is_approval_pending() {
            return;
        }
        if let Some(overlay) = self.parked_slash_overlay.take() {
            self.slash_overlay = overlay;
        }
    }
}
