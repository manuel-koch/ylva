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

#[test]
fn conversation_selections_use_terminal_columns_and_strip_chrome() {
    let mut state = AppState::new();
    let selection = ChatSelection {
        anchor_line: 0,
        anchor_col: 5,
        cursor_line: 0,
        cursor_col: 3,
    };
    state.conversation.chat_rendered_lines = vec!["a界b".into()];
    state.conversation.chat_rendered_line_prefix_widths = vec![2];
    state.conversation.chat_selection = Some(selection);
    state.subagent_view.header_rendered_lines = vec!["a界b".into()];
    state.subagent_view.header_rendered_line_prefix_widths = vec![2];
    state.subagent_view.header_selection = Some(selection);
    state.help_popup.help_rendered_lines = vec!["a界b".into()];
    state.help_popup.help_selection = Some(ChatSelection {
        anchor_col: 3,
        cursor_col: 1,
        ..selection
    });

    assert_eq!(state.extract_chat_selection_text().as_deref(), Some("界"));
    assert_eq!(state.extract_header_selection_text().as_deref(), Some("界"));
    assert_eq!(state.extract_help_selection_text().as_deref(), Some("界"));
}
