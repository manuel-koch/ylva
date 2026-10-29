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
use crate::tui_reducer_state::AgentRowKey;
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use std::time::Duration;
use uuid::Uuid;

fn row(label: &str, status: AgentRowStatus) -> AgentRow {
    AgentRow {
        key: AgentRowKey::Run(Uuid::new_v4()),
        label: label.to_string(),
        status,
        outcome: None,
        is_current: false,
        elapsed: None,
        awaiting_input: false,
        depth: 1,
        is_last_sibling: false,
    }
}

fn draw(state: &mut TuiState, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| draw_agents_popup(frame, state))
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn long_labels_are_elided_and_trailing_columns_stay_visible() {
    let mut running = row(&"x".repeat(200), AgentRowStatus::Running);
    running.elapsed = Some(Duration::from_secs(7));
    running.awaiting_input = true;
    let mut state = TuiState::new();
    state.agents_popup.rows = vec![running];
    let text = draw(&mut state, 80, 20);
    assert!(text.contains('…'));
    assert!(text.contains("7s"));
    assert!(text.contains(AWAITING_INPUT_TEXT));
}

#[test]
fn duration_is_flush_right_when_awaiting_input() {
    let mut plain = row("plain", AgentRowStatus::Running);
    plain.elapsed = Some(Duration::from_secs(7));
    let mut waiting = row("waiting", AgentRowStatus::Running);
    waiting.elapsed = Some(Duration::from_secs(7));
    waiting.awaiting_input = true;
    let mut state = TuiState::new();
    state.agents_popup.rows = vec![plain, waiting];
    let text = draw(&mut state, 80, 20);
    let lines: Vec<&str> = text.lines().filter(|line| line.contains("7s")).collect();
    assert_eq!(lines.len(), 2);
    for line in lines {
        assert!(line.trim_end().ends_with("7s│"), "{line:?}");
    }
}

#[test]
fn missing_elapsed_shows_dash_and_hitboxes_are_recorded() {
    let mut finished = row("replayed", AgentRowStatus::Finished);
    finished.is_current = true;
    let mut state = TuiState::new();
    state.agents_popup.rows = vec![row("queued job", AgentRowStatus::Queued), finished];
    state.agents_popup.selected_index = 1;
    let text = draw(&mut state, 80, 20);
    assert!(text.contains(NO_DURATION));
    assert!(text.contains(CURRENT_MARKER));
    assert_eq!(state.agents_popup.row_hitboxes.len(), 2);
    assert_eq!(state.agents_popup.visible_rows, 2);
}

#[test]
fn reveal_selection_scrolls_selected_row_into_view() {
    let mut state = TuiState::new();
    state.agents_popup.rows = (0..30)
        .map(|index| row(&format!("run {index}"), AgentRowStatus::Running))
        .collect();
    state.agents_popup.selected_index = 29;
    state.agents_popup.reveal_selection = true;
    let text = draw(&mut state, 80, 12);
    assert!(text.contains("run 29"));
    assert!(!state.agents_popup.reveal_selection);
    assert!(state.agents_popup.scroll_offset > 0);
}

#[test]
fn children_are_drawn_with_tree_branches() {
    let mut root = row("Root", AgentRowStatus::Root);
    root.depth = 0;
    root.is_last_sibling = true;
    let first = row("first", AgentRowStatus::Running);
    let mut last = row("last", AgentRowStatus::Finished);
    last.is_last_sibling = true;
    let mut state = TuiState::new();
    state.agents_popup.rows = vec![root, first, last];
    let text = draw(&mut state, 80, 20);
    assert!(text.contains("├─ first"));
    assert!(text.contains("└─ last"));
}

#[test]
fn status_and_duration_are_flush_right_with_a_three_space_gap() {
    let mut waiting = row("Copy the file", AgentRowStatus::Running);
    waiting.awaiting_input = true;
    waiting.elapsed = Some(Duration::from_secs(150));
    let mut running = row("Other topic", AgentRowStatus::Running);
    running.elapsed = Some(Duration::from_secs(190));
    let mut finished = row("Super duper task", AgentRowStatus::Finished);
    finished.outcome = Some(SubagentOutcome::Success);
    finished.elapsed = Some(Duration::from_secs(15));
    let mut state = TuiState::new();
    state.agents_popup.rows = vec![waiting, running, finished];
    let text = draw(&mut state, 100, 8);
    let lines: Vec<&str> = text.lines().filter(|line| line.contains("s│")).collect();
    assert_eq!(lines.len(), 3);
    assert!(
        lines[0].contains("waiting for user input   2m30s│"),
        "{text}"
    );
    assert!(lines[1].contains("running   3m10s│"), "{text}");
    assert!(lines[2].contains("success     15s│"), "{text}");
}

#[test]
fn overflowing_rows_reserve_a_scrollbar_column_at_the_right_edge() {
    let mut state = TuiState::new();
    state.agents_popup.rows = (0..20)
        .map(|index| {
            let mut agent = row(&format!("Agent {index}"), AgentRowStatus::Running);
            agent.elapsed = Some(Duration::from_secs(5));
            agent
        })
        .collect();
    let text = draw(&mut state, 100, 10);
    assert!(text.contains("5s↑│") && text.contains("5s↓│"), "{text}");
    let hitbox = state.agents_popup.row_hitboxes[0].0;
    assert_eq!(hitbox.width, state.agents_popup.popup_rect.width - 3);

    state.agents_popup.rows.truncate(3);
    let text = draw(&mut state, 100, 10);
    assert!(!text.contains("↑") && !text.contains("┃"), "{text}");
}
