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
fn history_navigation_preserves_the_draft_across_both_boundaries() {
    let mut state = AppState::new();
    state.prepend_input_history(vec!["oldest".into(), "newest".into()]);

    assert_eq!(
        state.navigate_input_message_history(HistoryDirection::Down, "draft"),
        None
    );
    assert_eq!(
        state.navigate_input_message_history(HistoryDirection::Up, "draft"),
        Some("newest".into())
    );
    assert_eq!(
        state.navigate_input_message_history(HistoryDirection::Up, "newest"),
        Some("oldest".into())
    );
    assert_eq!(
        state.navigate_input_message_history(HistoryDirection::Up, "oldest"),
        None
    );
    assert_eq!(
        state.navigate_input_message_history(HistoryDirection::Down, "oldest"),
        Some("newest".into())
    );
    assert_eq!(
        state.navigate_input_message_history(HistoryDirection::Down, "newest"),
        Some("draft".into())
    );
    assert_eq!(state.history_index, None);
}

#[test]
fn editing_a_recalled_message_exits_browsing_without_changing_history() {
    let mut state = AppState::new();
    state.input_history.push("previous".into());
    state.navigate_input_message_history(HistoryDirection::Up, "draft");
    state.exit_input_history_browsing("edited");

    assert_eq!(state.history_index, None);
    assert_eq!(state.draft, "edited");
    assert_eq!(state.input_history, ["previous"]);
}
