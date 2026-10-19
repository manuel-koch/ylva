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

use std::time::Instant;

use super::super::*;
use crate::agent::AgentMode;

impl AppState {
    /// Returns `true` if a toast is active and has not yet expired.
    pub fn has_active_toast(&self) -> bool {
        self.toast
            .as_ref()
            .map(|(_, start)| start.elapsed() < TOAST_DURATION)
            .unwrap_or(false)
    }

    /// The focus to take when the help popup closes. An outstanding tool
    /// approval outranks the normal input: returning to `Focus::MainInput`
    /// there would hand the slash-command-capable main input to the user
    /// while a tool still awaits its verdict (and hide the approval
    /// controls), so the approval focus is resumed instead.
    pub fn restore_focus_after_help(&mut self) {
        self.focus = Focus::MainInput;
        self.resume_pending_tool_approval();
    }

    /// Scroll the help popup up.
    pub fn help_scroll_up(&mut self, lines: usize) {
        self.help_popup.help_scroll_offset =
            self.help_popup.help_scroll_offset.saturating_sub(lines);
        self.clear_help_selection();
    }

    /// Scroll the help popup down.
    pub fn help_scroll_down(&mut self, lines: usize) {
        self.help_popup.help_scroll_offset =
            self.help_popup.help_scroll_offset.saturating_add(lines);
        self.clear_help_selection();
    }

    /// Scroll the help popup left.
    pub fn help_scroll_left(&mut self, columns: usize) {
        self.help_popup.help_scroll_x = self.help_popup.help_scroll_x.saturating_sub(columns);
        self.clear_help_selection();
    }

    /// Scroll the help popup right.
    pub fn help_scroll_right(&mut self, columns: usize) {
        self.help_popup.help_scroll_x = self.help_popup.help_scroll_x.saturating_add(columns);
        self.clear_help_selection();
    }

    pub fn clear_help_selection(&mut self) {
        self.help_popup.help_selection = None;
        self.help_popup.help_drag_active = false;
    }

    /// Scroll the clarify banner up (toward earlier content).
    pub fn clarify_scroll_up(&mut self, lines: usize) {
        self.clarify_banner_state.clarify_scroll_offset = self
            .clarify_banner_state
            .clarify_scroll_offset
            .saturating_sub(lines);
    }

    /// Scroll the clarify banner down (toward later content). The upper bound
    /// is applied in `draw_clarify_banner`, which knows the total content height.
    pub fn clarify_scroll_down(&mut self, lines: usize) {
        self.clarify_banner_state.clarify_scroll_offset = self
            .clarify_banner_state
            .clarify_scroll_offset
            .saturating_add(lines);
    }

    /// Show a transient toast notification.
    pub fn set_toast(&mut self, message: impl Into<String>) {
        self.toast = Some((message.into(), Instant::now() + TOAST_DURATION));
    }

    // ── View-layer helpers ────────────────────────────────────────────────────

    /// Convert the current [`TuiMode`] (and [`Focus`]) to the legacy
    /// [`AgentMode`] used by `tui_widgets.rs` for footer and spinner rendering.
    pub fn agent_mode(&self) -> AgentMode {
        match &self.mode {
            // A spawned subagent keeps its `spawn_subagent` tool bubble in
            // `Running` until the subagent actually finishes, independently of
            // the root loop's own `TuiMode` — the root can go `Idle` (its
            // own turn ended) while the subagent is still working. Report
            // `ToolRunning` in that case so the footer/spinner doesn't look
            // finished while a subagent is still active.
            TuiMode::Idle => {
                if self.has_running_tool_bubble() {
                    AgentMode::ToolRunning
                } else {
                    AgentMode::Idle
                }
            }
            TuiMode::Pending => AgentMode::Pending,
            TuiMode::Streaming => {
                if matches!(self.focus, Focus::ToolApproval { .. }) {
                    AgentMode::ToolPending
                } else if self.has_running_tool_bubble() {
                    AgentMode::ToolRunning
                } else {
                    AgentMode::Streaming
                }
            }
            TuiMode::ClarifyPending { .. } => AgentMode::ClarifyPending,
            TuiMode::Aborting => AgentMode::Aborting,
            TuiMode::DoublePressAbort { return_to, .. } => match return_to.as_ref() {
                TuiMode::Idle | TuiMode::DoublePressAbort { .. } => {
                    if self.has_running_tool_bubble() {
                        AgentMode::ToolRunning
                    } else {
                        AgentMode::Idle
                    }
                }
                TuiMode::Pending => AgentMode::Pending,
                TuiMode::Streaming => {
                    if self.has_running_tool_bubble() {
                        AgentMode::ToolRunning
                    } else {
                        AgentMode::Streaming
                    }
                }
                TuiMode::ClarifyPending { .. } => AgentMode::ClarifyPending,
                TuiMode::Aborting => AgentMode::Aborting,
            },
        }
    }

    /// Returns `true` when the help popup is currently displayed.
    pub fn show_help(&self) -> bool {
        matches!(self.focus, Focus::HelpPopup)
    }

    /// Returns `true` when a tool is awaiting user approval.
    pub fn is_approval_pending(&self) -> bool {
        matches!(self.focus, Focus::ToolApproval { .. })
    }

    /// Returns the name of the tool awaiting approval, or `None` when not in
    /// approval sub-mode.
    pub fn approval_tool_name(&self) -> Option<&str> {
        if let Focus::ToolApproval { tool_name, .. } = &self.focus {
            Some(tool_name.as_str())
        } else {
            None
        }
    }
}
