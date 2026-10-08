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

//! Input history state and operations.

use super::AppState;

/// Direction for [`AppState::navigate_input_message_history`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryDirection {
    Up,
    Down,
}

impl AppState {
    /// Prepend messages loaded from prior sessions to the current input
    /// history without changing the hydrated conversation view.
    pub fn prepend_input_history(&mut self, mut prior_messages: Vec<String>) {
        prior_messages.append(&mut self.input_history);
        self.input_history = prior_messages;
        self.history_index = None;
    }

    /// Navigate input history. Returns the text to load, or `None` if boundary reached.
    pub fn navigate_input_message_history(
        &mut self,
        direction: HistoryDirection,
        current_text: &str,
    ) -> Option<String> {
        match direction {
            HistoryDirection::Up => {
                if self.input_history.is_empty() {
                    return None;
                }
                match self.history_index {
                    None => {
                        self.draft = current_text.to_string();
                        self.history_index = Some(self.input_history.len() - 1);
                    }
                    Some(0) => return None,
                    Some(index) => {
                        self.history_index = Some(index - 1);
                    }
                }
                Some(self.input_history[self.history_index.expect("just set")].clone())
            }
            HistoryDirection::Down => match self.history_index {
                None => None,
                Some(index) if index < self.input_history.len() - 1 => {
                    self.history_index = Some(index + 1);
                    Some(self.input_history[index + 1].clone())
                }
                Some(_) => {
                    self.history_index = None;
                    Some(std::mem::take(&mut self.draft))
                }
            },
        }
    }

    /// Exit history browsing (if active) before an edit keypress.
    pub fn exit_input_history_browsing(&mut self, current_text: &str) {
        if self.history_index.is_some() {
            self.draft = current_text.to_string();
            self.history_index = None;
        }
    }
}
