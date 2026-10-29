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

//! Agents popup (`/agents`, `ctrl-a`): row model, selection and activation.

use super::{
    AppState, ChatEntry, ConversationView, DoubleClickTracker, Effect, Focus, SubagentOutcome,
    ToolCallState,
};
use crate::agent::subagent_manager::STATUS_QUEUED;
use crate::agent::tools::clarify::ClarifyTool;
use crate::agent::tools::spawn_subagent::SpawnSubagentTool;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Label of the pinned first row.
pub const AGENTS_POPUP_ROOT_LABEL: &str = "Root (main conversation)";

/// Stable identity of a popup row. A queued run is identified by its root
/// `spawn_subagent` bubble because no run exists yet; once it starts, the
/// selection follows it to its [`AgentRowKey::Run`] row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentRowKey {
    Root,
    Run(Uuid),
    Queued(Uuid),
}

/// Lifecycle group of a popup row; also the display order of the groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRowStatus {
    Root,
    Running,
    Queued,
    Finished,
}

/// One displayed line of the agents popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRow {
    pub key: AgentRowKey,
    pub label: String,
    pub status: AgentRowStatus,
    /// Outcome of a finished run; `None` for replayed runs.
    pub outcome: Option<SubagentOutcome>,
    /// The conversation currently shown in the main chat area.
    pub is_current: bool,
    /// Live while running, frozen once finished; `None` for the root, queued
    /// and replayed runs.
    pub elapsed: Option<Duration>,
    /// The run (or root) has an approval or clarify question outstanding.
    pub awaiting_input: bool,
    /// Nesting level in the tree view: 0 for the root, 1 for its subagents.
    pub depth: usize,
    /// Last child of its parent, drawn with `└─` instead of `├─`.
    pub is_last_sibling: bool,
}

impl AgentRow {
    /// Queued runs have no conversation yet, so they cannot be selected.
    pub fn is_selectable(&self) -> bool {
        self.status != AgentRowStatus::Queued
    }
}

/// Selection and scroll state of the agents popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentsPopupState {
    /// Tracked by key so reordering rows never moves the highlight.
    pub selected: AgentRowKey,
    pub scroll_offset: usize,
    /// Set when the renderer must scroll the selected row into view; cleared
    /// by the renderer once done.
    pub reveal_selection: bool,
    pub last_click: DoubleClickTracker<AgentRowKey>,
}

impl Default for AgentsPopupState {
    fn default() -> Self {
        AgentsPopupState {
            selected: AgentRowKey::Root,
            scroll_offset: 0,
            reveal_selection: false,
            last_click: DoubleClickTracker::default(),
        }
    }
}

fn transcript_awaits_input(messages: &[ChatEntry]) -> bool {
    messages.iter().any(|entry| match entry {
        ChatEntry::ToolBubble { name, state, .. } => match state.as_ref() {
            ToolCallState::Pending { .. } => true,
            ToolCallState::Running { .. } => name == ClarifyTool::NAME,
            _ => false,
        },
        _ => false,
    })
}

fn queued_label(title_arguments: &[(String, serde_json::Value)]) -> String {
    title_arguments
        .iter()
        .find_map(|(key, value)| (key == "label").then(|| value.as_str()))
        .flatten()
        .map(str::to_owned)
        .unwrap_or_else(|| "Queued subagent".to_string())
}

impl AppState {
    /// Rows of the agents popup as a tree (the root with its subagents as
    /// children) in display order: root, running (oldest
    /// first), queued (oldest first), finished (newest first).
    pub fn agent_rows(&self, now: Instant) -> Vec<AgentRow> {
        let current_run_id = match &self.active_view {
            ConversationView::Root => None,
            ConversationView::Subagent { run_id, .. } => Some(*run_id),
        };
        let bubble_positions: HashMap<Uuid, usize> = self
            .chat_entries
            .iter()
            .enumerate()
            .filter_map(|(position, entry)| match entry {
                ChatEntry::ToolBubble { id, .. } => Some((*id, position)),
                _ => None,
            })
            .collect();
        let position_of = |bubble_id: Uuid| {
            bubble_positions
                .get(&bubble_id)
                .copied()
                .unwrap_or(usize::MAX)
        };

        let mut running = Vec::new();
        let mut finished = Vec::new();
        for (run_id, conversation) in &self.subagent_conversations {
            let is_running = self.running_subagents.contains_key(run_id);
            let elapsed = match (
                conversation.started_at,
                is_running,
                conversation.finished_at,
            ) {
                (Some(started_at), true, _) => Some(now.saturating_duration_since(started_at)),
                (Some(started_at), false, Some(finished_at)) => {
                    Some(finished_at.saturating_duration_since(started_at))
                }
                _ => conversation.recorded_duration,
            };
            let row = AgentRow {
                key: AgentRowKey::Run(*run_id),
                label: conversation.info.label.clone(),
                status: if is_running {
                    AgentRowStatus::Running
                } else {
                    AgentRowStatus::Finished
                },
                outcome: conversation.outcome,
                is_current: current_run_id == Some(*run_id),
                elapsed,
                awaiting_input: is_running && transcript_awaits_input(&conversation.messages),
                depth: 1,
                is_last_sibling: false,
            };
            let position = position_of(conversation.info.bubble_id);
            if is_running {
                running.push((conversation.started_at, position, row));
            } else {
                finished.push((conversation.finished_at, position, row));
            }
        }
        running.sort_by_key(|(started_at, position, _)| (*started_at, *position));
        // Newest first; replayed runs (no finish time) follow, latest bubble first.
        finished.sort_by(|left, right| {
            right
                .0
                .is_some()
                .cmp(&left.0.is_some())
                .then(right.0.cmp(&left.0))
                .then(right.1.cmp(&left.1))
        });

        let known_bubbles: Vec<Uuid> = self
            .subagent_conversations
            .values()
            .map(|conversation| conversation.info.bubble_id)
            .collect();
        let queued = self.chat_entries.iter().filter_map(|entry| {
            let ChatEntry::ToolBubble {
                id, name, state, ..
            } = entry
            else {
                return None;
            };
            let ToolCallState::Running {
                title_arguments,
                progress_arguments,
                ..
            } = state.as_ref()
            else {
                return None;
            };
            let is_queued = name == SpawnSubagentTool::NAME
                && progress_arguments
                    .iter()
                    .any(|(key, value)| key == "status" && value.as_str() == Some(STATUS_QUEUED))
                && !known_bubbles.contains(id);
            is_queued.then(|| AgentRow {
                key: AgentRowKey::Queued(*id),
                label: queued_label(title_arguments),
                status: AgentRowStatus::Queued,
                outcome: None,
                is_current: false,
                elapsed: None,
                awaiting_input: false,
                depth: 1,
                is_last_sibling: false,
            })
        });

        let root = AgentRow {
            key: AgentRowKey::Root,
            label: self
                .session_title
                .as_deref()
                .filter(|title| !title.trim().is_empty())
                .map_or_else(|| AGENTS_POPUP_ROOT_LABEL.to_string(), str::to_string),
            status: AgentRowStatus::Root,
            outcome: None,
            is_current: current_run_id.is_none(),
            elapsed: None,
            awaiting_input: transcript_awaits_input(&self.chat_entries),
            depth: 0,
            is_last_sibling: true,
        };

        let mut rows: Vec<AgentRow> = std::iter::once(root)
            .chain(running.into_iter().map(|(_, _, row)| row))
            .chain(queued)
            .chain(finished.into_iter().map(|(_, _, row)| row))
            .collect();
        if let Some(last_child) = rows.iter_mut().skip(1).next_back() {
            last_child.is_last_sibling = true;
        }
        rows
    }

    /// Index of the highlighted row. A queued selection whose run has since
    /// started follows it; any other vanished selection falls back to root.
    pub fn agents_popup_selected_index(&self, rows: &[AgentRow]) -> usize {
        let selected = self.agents_popup.selected;
        if let Some(index) = rows.iter().position(|row| row.key == selected) {
            return index;
        }
        if let AgentRowKey::Queued(bubble_id) = selected {
            if let Some(run_id) = self.run_id_for_bubble(bubble_id) {
                if let Some(index) = rows
                    .iter()
                    .position(|row| row.key == AgentRowKey::Run(run_id))
                {
                    return index;
                }
            }
        }
        0
    }

    /// Opens (or switches to) the popup with the viewed conversation highlighted.
    pub fn open_agents_popup(&mut self) {
        self.focus = Focus::AgentsPopup;
        self.clear_help_selection();
        self.agents_popup.selected = match &self.active_view {
            ConversationView::Root => AgentRowKey::Root,
            ConversationView::Subagent { run_id, .. } => AgentRowKey::Run(*run_id),
        };
        self.agents_popup.scroll_offset = 0;
        self.agents_popup.reveal_selection = true;
        self.agents_popup.last_click.reset();
    }

    /// Closes the popup, resuming an outstanding approval like the help popup.
    pub fn close_agents_popup(&mut self) {
        self.restore_focus_after_help();
        self.agents_popup.last_click.reset();
    }

    /// Moves the highlight by `steps` selectable rows (negative = up),
    /// clamped to the first/last selectable row.
    pub fn agents_popup_move_selection(&mut self, steps: isize, now: Instant) {
        let rows = self.agent_rows(now);
        let selectable: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| row.is_selectable().then_some(index))
            .collect();
        let current = self.agents_popup_selected_index(&rows);
        let position = selectable
            .iter()
            .position(|index| *index >= current)
            .unwrap_or(selectable.len().saturating_sub(1));
        let target = (position as isize + steps).clamp(0, selectable.len() as isize - 1);
        self.agents_popup.selected = rows[selectable[target as usize]].key;
        self.agents_popup.reveal_selection = true;
    }

    /// Highlights `key` if it is a selectable row; returns whether it was a
    /// second click on the same row within [`DOUBLE_CLICK_WINDOW`].
    pub fn agents_popup_click(&mut self, key: AgentRowKey, now: Instant) -> bool {
        let rows = self.agent_rows(now);
        if !rows.iter().any(|row| row.key == key && row.is_selectable()) {
            return false;
        }
        self.agents_popup.selected = key;
        self.agents_popup.last_click.register(key, now)
    }

    pub fn agents_popup_scroll(&mut self, rows: isize) {
        self.agents_popup.scroll_offset =
            self.agents_popup.scroll_offset.saturating_add_signed(rows);
        self.agents_popup.reveal_selection = false;
    }

    /// Switches to the highlighted agent and closes the popup. Queued rows
    /// cannot be activated. Returns the lazy transcript load effect when the
    /// target is a replayed run whose transcript is not loaded yet.
    pub fn activate_agents_popup_selection(&mut self, now: Instant) -> Vec<Effect> {
        let rows = self.agent_rows(now);
        let index = self.agents_popup_selected_index(&rows);
        let row = &rows[index];
        if !row.is_selectable() {
            return vec![];
        }
        let target = row.key;
        let is_current = row.is_current;
        self.close_agents_popup();
        if is_current {
            return vec![];
        }
        match target {
            AgentRowKey::Root => self.switch_to_root_view(),
            AgentRowKey::Run(run_id) => {
                if self.subagent_needs_lazy_load(run_id) {
                    return vec![Effect::LoadSubagentTranscript { run_id }];
                }
                self.switch_to_subagent_view(run_id);
            }
            AgentRowKey::Queued(_) => {}
        }
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui_reducer_state::{SubagentConversation, SubagentRunInfo};

    fn run_info(label: &str) -> SubagentRunInfo {
        SubagentRunInfo {
            run_id: Uuid::new_v4(),
            bubble_id: Uuid::new_v4(),
            agent_type: "default".to_string(),
            task: "task".to_string(),
            label: label.to_string(),
        }
    }

    #[test]
    fn rows_list_root_then_running_then_finished_with_current_marker() {
        let mut state = AppState::new();
        let finished = run_info("done one");
        let running = run_info("busy one");
        state.start_subagent_run(finished.clone());
        state.running_subagents.remove(&finished.run_id);
        let conversation = state
            .subagent_conversations
            .get_mut(&finished.run_id)
            .unwrap();
        conversation.finished_at = Some(Instant::now());
        conversation.outcome = Some(SubagentOutcome::Success);
        state.start_subagent_run(running.clone());

        let rows = state.agent_rows(Instant::now());
        let keys: Vec<_> = rows.iter().map(|row| row.key).collect();
        assert_eq!(
            keys,
            vec![
                AgentRowKey::Root,
                AgentRowKey::Run(running.run_id),
                AgentRowKey::Run(finished.run_id)
            ]
        );
        assert!(rows[0].is_current);
        assert_eq!(rows[1].status, AgentRowStatus::Running);
        assert_eq!(rows[2].status, AgentRowStatus::Finished);
        assert_eq!(rows[2].outcome, Some(SubagentOutcome::Success));
        assert_eq!(
            rows.iter().map(|row| row.depth).collect::<Vec<_>>(),
            [0, 1, 1]
        );
        assert_eq!(
            rows.iter()
                .map(|row| row.is_last_sibling)
                .collect::<Vec<_>>(),
            [true, false, true]
        );
    }

    #[test]
    fn selection_moves_over_rows_and_clamps() {
        let mut state = AppState::new();
        state.start_subagent_run(run_info("one"));
        state.start_subagent_run(run_info("two"));
        state.open_agents_popup();
        let now = Instant::now();
        let rows = state.agent_rows(now);
        assert_eq!(state.agents_popup_selected_index(&rows), 0);
        state.agents_popup_move_selection(1, now);
        state.agents_popup_move_selection(5, now);
        assert_eq!(state.agents_popup_selected_index(&rows), 2);
        state.agents_popup_move_selection(-9, now);
        assert_eq!(state.agents_popup_selected_index(&rows), 0);
    }

    #[test]
    fn root_row_uses_the_session_title_once_generated() {
        let mut state = AppState::new();
        let label = |state: &AppState| state.agent_rows(Instant::now())[0].label.clone();
        assert_eq!(label(&state), AGENTS_POPUP_ROOT_LABEL);
        state.session_title = Some("  ".to_string());
        assert_eq!(label(&state), AGENTS_POPUP_ROOT_LABEL);
        state.session_title = Some("Fix the parser".to_string());
        assert_eq!(label(&state), "Fix the parser");
    }

    #[test]
    fn activating_a_run_switches_view_and_root_returns() {
        let mut state = AppState::new();
        let info = run_info("one");
        state.start_subagent_run(info.clone());
        state.open_agents_popup();
        state.agents_popup_move_selection(1, Instant::now());
        state.activate_agents_popup_selection(Instant::now());
        assert!(!state.show_agents_popup());
        assert!(matches!(
            state.active_view,
            ConversationView::Subagent { run_id, .. } if run_id == info.run_id
        ));

        state.open_agents_popup();
        state.agents_popup_move_selection(-1, Instant::now());
        state.activate_agents_popup_selection(Instant::now());
        assert!(matches!(state.active_view, ConversationView::Root));
    }

    #[test]
    fn activating_a_replayed_run_requests_lazy_transcript_load() {
        let mut state = AppState::new();
        let info = run_info("old");
        state
            .subagent_conversations
            .insert(info.run_id, SubagentConversation::new_replay(info.clone()));
        state.open_agents_popup();
        state.agents_popup_move_selection(1, Instant::now());
        let effects = state.activate_agents_popup_selection(Instant::now());
        assert!(effects
            .iter()
            .any(|effect| matches!(effect, Effect::LoadSubagentTranscript { .. })));
    }

    #[test]
    fn second_click_within_window_is_a_double_click() {
        let mut state = AppState::new();
        state.start_subagent_run(run_info("one"));
        state.open_agents_popup();
        let key = AgentRowKey::Root;
        let now = Instant::now();
        assert!(!state.agents_popup_click(key, now));
        assert!(state.agents_popup_click(key, now + Duration::from_millis(50)));
    }
}
