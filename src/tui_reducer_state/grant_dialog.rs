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

//! Grant dialog: the second page of the tool-approval prompt, where the user
//! reviews the proposed grant, picks its lifetime and confirms or goes back.

use super::{AppState, Effect, Focus};
use crate::agent::GrantPreview;
use crate::grants::GrantLifetime;

/// A button of the grant dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantDialogButton {
    Confirm,
    Back,
}

/// The open grant dialog for the pending approval `preview.id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantDialogState {
    pub preview: GrantPreview,
    pub lifetime: GrantLifetime,
    pub focused: GrantDialogButton,
    /// First visible body row; the renderer clamps it to the content.
    pub scroll_offset: usize,
}

impl GrantDialogState {
    /// Whether the selected lifetime can be created for this call.
    pub fn can_confirm(&self) -> bool {
        self.preview.refusal.is_none()
            && (self.lifetime != GrantLifetime::Persistent
                || self.preview.persistent_unavailable.is_none())
    }
}

impl AppState {
    /// Opens the dialog over the approval prompt it was requested for. Ignored
    /// when that prompt is no longer the pending approval.
    pub fn open_grant_dialog(&mut self, preview: GrantPreview) {
        let pending =
            matches!(&self.focus, Focus::ToolApproval { tool_id, .. } if *tool_id == preview.id);
        if !pending {
            return;
        }
        self.grant_dialog = Some(GrantDialogState {
            preview,
            lifetime: GrantLifetime::Temporary,
            focused: GrantDialogButton::Confirm,
            scroll_offset: 0,
        });
    }

    /// Returns to the approval prompt.
    pub fn close_grant_dialog(&mut self) {
        self.grant_dialog = None;
    }

    /// The dialog, only while its approval is still the pending one.
    pub fn visible_grant_dialog(&self) -> Option<&GrantDialogState> {
        let dialog = self.grant_dialog.as_ref()?;
        match &self.focus {
            Focus::ToolApproval { tool_id, .. } if *tool_id == dialog.preview.id => Some(dialog),
            _ => None,
        }
    }

    /// Selects `lifetime`; a persistent grant that cannot be saved is not selectable.
    pub fn select_grant_lifetime(&mut self, lifetime: GrantLifetime) {
        if let Some(dialog) = self.grant_dialog.as_mut() {
            if lifetime == GrantLifetime::Persistent
                && dialog.preview.persistent_unavailable.is_some()
            {
                return;
            }
            dialog.lifetime = lifetime;
        }
    }

    /// Scrolls the body by `rows` (negative = up), within the content drawn last.
    pub fn scroll_grant_dialog(&mut self, rows: isize) {
        let maximum = self.layout.grant_dialog.maximum_scroll;
        if let Some(dialog) = self.grant_dialog.as_mut() {
            dialog.scroll_offset = dialog
                .scroll_offset
                .min(maximum)
                .saturating_add_signed(rows)
                .min(maximum);
        }
    }

    pub fn toggle_grant_lifetime(&mut self) {
        let next = match self.grant_dialog.as_ref().map(|dialog| dialog.lifetime) {
            Some(GrantLifetime::Temporary) => GrantLifetime::Persistent,
            Some(GrantLifetime::Persistent) => GrantLifetime::Temporary,
            None => return,
        };
        self.select_grant_lifetime(next);
    }

    pub fn toggle_grant_dialog_button(&mut self) {
        if let Some(dialog) = self.grant_dialog.as_mut() {
            dialog.focused = match dialog.focused {
                GrantDialogButton::Confirm => GrantDialogButton::Back,
                GrantDialogButton::Back => GrantDialogButton::Confirm,
            };
        }
    }

    /// Confirms the selected lifetime. The dialog closes and the agent creates
    /// the grant; a grant that cannot be created is explained by a notice and
    /// leaves the approval prompt pending. Does nothing when the selection
    /// cannot be confirmed.
    pub fn confirm_grant_dialog(&mut self) -> Vec<Effect> {
        let Some(dialog) = self
            .visible_grant_dialog()
            .filter(|dialog| dialog.can_confirm())
        else {
            return vec![];
        };
        let effect = Effect::GrantTool {
            id: dialog.preview.id,
            lifetime: dialog.lifetime,
        };
        self.grant_dialog = None;
        vec![effect]
    }

    /// Activates the keyboard-focused dialog button.
    pub fn activate_grant_dialog_button(&mut self) -> Vec<Effect> {
        match self.grant_dialog.as_ref().map(|dialog| dialog.focused) {
            Some(GrantDialogButton::Confirm) => self.confirm_grant_dialog(),
            Some(GrantDialogButton::Back) => {
                self.close_grant_dialog();
                vec![]
            }
            None => vec![],
        }
    }
}
