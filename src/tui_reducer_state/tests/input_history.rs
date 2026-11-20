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

#[test]
fn cursor_movement_keeps_browsing_active() {
    // Pure cursor movement must not end history browsing: `←/→` and the
    // in-draft `↑/↓` just move the cursor, and `↓` at the bottom must
    // continue walking the history as if no cursor move had happened. The
    // Presenter forwards cursor keys to the textarea without an exit
    // effect, so the browse session stays active until an actual edit.
    let mut state = AppState::new();
    state.input_history.push("previous".into());
    state.navigate_input_message_history(HistoryDirection::Up, "the saved draft");
    assert_eq!(state.history_index, Some(0));

    // Browsing remains active; `↓` at the bottom of the recalled entry
    // still restores the saved draft once (the browsing session boundary).
    let restored = state.navigate_input_message_history(HistoryDirection::Down, "previous");
    assert_eq!(restored.as_deref(), Some("the saved draft"));
    assert_eq!(state.history_index, None);
    assert_eq!(state.draft, "");
}
#[test]
fn down_at_boundary_restores_draft_and_keeps_it_recoverable() {
    // The boundary contract: with a single history entry, `↑` recalls it
    // (saving the live draft), and `↓` on its last row hands the saved
    // draft back. The draft is consumed by that restore — the session is
    // back at the live draft, so a further `↓` has nothing to recall.
    let mut state = AppState::new();
    state.input_history = vec!["entry a".into()];
    let restored = state.navigate_input_message_history(HistoryDirection::Up, "my wrapped draft");
    assert_eq!(restored.as_deref(), Some("entry a"));

    // `↓` from the last row of "entry a" restores the draft and exits
    // browsing…
    let restored = state.navigate_input_message_history(HistoryDirection::Down, "entry a");
    assert_eq!(restored.as_deref(), Some("my wrapped draft"));
    assert_eq!(state.history_index, None);
    assert_eq!(state.draft, "", "restore hands the drafted text over once");

    // …and a further `↓` on the restored (now live) draft has nothing to
    // recall — it is a normal no-op, not an endless re-recall.
    let again = state.navigate_input_message_history(HistoryDirection::Down, "my wrapped draft");
    assert_eq!(again, None);
    assert_eq!(state.history_index, None);
}
