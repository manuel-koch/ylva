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
use crate::context_breakdown::{ContextCategory, ContextSegment};
use crate::theme::status_free_segment_color;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn snapshot(segments: Vec<ContextSegment>, context_length: usize) -> StatusSnapshot {
    StatusSnapshot {
        run_id: None,
        engine_name: "main".to_string(),
        provider_connection: "ollama-local".to_string(),
        model: "qwen3".to_string(),
        reasoning_effort: None,
        temperature: Some("0.2".to_string()),
        max_output_tokens: None,
        agent_type: Some("default".to_string()),
        allowed_tools: vec!["read".to_string(), "write".to_string()],
        tool_call_counts: Default::default(),
        breakdown: ContextBreakdown::new(segments, context_length),
        timeline: Vec::new(),
        finished_subagent: false,
    }
}

fn segment(category: ContextCategory, tokens: usize) -> ContextSegment {
    ContextSegment { category, tokens }
}

fn line_text(line: &Line) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn texts(snapshot: &StatusSnapshot, width: usize) -> Vec<String> {
    status_lines(snapshot, width)
        .iter()
        .map(line_text)
        .collect()
}

fn draw(state: &mut TuiState, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| draw_status_popup(frame, state))
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
fn unreported_engine_parameters_are_omitted() {
    let lines = texts(&snapshot(vec![], 1000), 60);

    assert!(lines.iter().any(|line| line.starts_with("Temperature")));
    assert!(!lines.iter().any(|line| line.starts_with("Reasoning")));
    assert!(!lines.iter().any(|line| line.starts_with("Max output")));
    assert!(lines.iter().any(|line| line.starts_with("Context window")));
}

#[test]
fn the_legend_lists_percentages_of_the_window_and_the_free_remainder() {
    let lines = texts(
        &snapshot(
            vec![
                segment(ContextCategory::UserMessages, 250),
                segment(ContextCategory::ToolCalls, 500),
            ],
            1000,
        ),
        60,
    );

    assert!(lines
        .iter()
        .any(|line| line.contains("User messages") && line.contains("25.0%")));
    assert!(lines
        .iter()
        .any(|line| line.contains("Tool calls") && line.contains("50.0%")));
    assert!(lines
        .iter()
        .any(|line| line.contains("Free") && line.contains("25.0%")));
    assert!(lines.iter().any(|line| line.contains("75.0% of window")));
}

#[test]
fn the_contribution_table_has_titles_and_a_percentage_of_used_column() {
    let lines = texts(
        &snapshot(
            vec![
                segment(ContextCategory::UserMessages, 250),
                segment(ContextCategory::ToolCalls, 500),
            ],
            1000,
        ),
        70,
    );

    let header = lines
        .iter()
        .find(|line| line.contains("Contribution"))
        .expect("table header");
    assert!(header.contains("% window") && header.contains("% used") && header.contains("Tokens"));
    assert!(lines.iter().any(|line| line.contains("User messages")
        && line.contains("25.0%")
        && line.contains("33.3%")));
    assert!(lines.iter().any(|line| line.contains("Tool calls")
        && line.contains("50.0%")
        && line.contains("66.7%")));
    let free = lines.iter().find(|line| line.contains("Free")).unwrap();
    assert!(free.contains("25.0%") && free.contains(" -"));
    assert!(lines
        .iter()
        .any(|line| line.contains("Relative to used context size")));
}

#[test]
fn the_used_bar_is_filled_even_when_the_window_is_mostly_free() {
    let breakdown = ContextBreakdown::new(vec![segment(ContextCategory::UserMessages, 100)], 1000);

    assert_eq!(line_text(&used_bar(&breakdown, 20)), "█".repeat(20));
}

#[test]
fn every_segment_of_a_dozen_contributions_has_its_own_colour() {
    let palette = segment_palette();
    for (index, color) in palette.iter().enumerate() {
        assert!(!palette[..index].contains(color), "duplicate at {index}");
        assert_ne!(*color, status_free_segment_color());
    }
}

#[test]
fn the_bar_fills_the_width_with_a_free_tail() {
    let bar = line_text(&usage_bar(
        &ContextBreakdown::new(vec![segment(ContextCategory::UserMessages, 500)], 1000),
        20,
    ));

    assert_eq!(bar, format!("{}{}", "█".repeat(10), "░".repeat(10)));
}

#[test]
fn an_overshooting_estimate_leaves_no_free_cells() {
    let bar = line_text(&usage_bar(
        &ContextBreakdown::new(vec![segment(ContextCategory::UserMessages, 2000)], 1000),
        20,
    ));

    assert_eq!(bar, "█".repeat(20));
}

#[test]
fn long_tool_lists_wrap_within_the_value_column() {
    let mut long = snapshot(vec![], 1000);
    long.allowed_tools = (0..12)
        .map(|index| format!("tool_number_{index}"))
        .collect();

    let lines = texts(&long, 50);

    let tool_lines: Vec<&String> = lines
        .iter()
        .skip_while(|line| !line.starts_with("Tools"))
        .take_while(|line| !line.is_empty())
        .collect();
    assert!(tool_lines.len() > 1);
    assert!(tool_lines.iter().all(|line| line.chars().count() <= 50));
}

#[test]
fn the_popup_draws_and_reports_its_rect_and_clamps_scroll() {
    let mut state = TuiState::new();
    state.status_popup.snapshot = Some(snapshot(
        vec![segment(ContextCategory::UserMessages, 100)],
        1000,
    ));
    state.status_popup.scroll_offset = 999;

    let text = draw(&mut state, 80, 40);

    assert!(text.contains("Status"));
    assert!(text.contains("Current setup"));
    assert_eq!(
        state.status_popup.popup_rect.width, 74,
        "terminal width - 6"
    );
    assert_eq!(state.status_popup.popup_rect.x, 3);
    assert_eq!(state.status_popup.scroll_offset, 0);
}

#[test]
fn the_popup_spans_the_conversation_height_and_pads_the_scrollbar() {
    let mut state = TuiState::new();
    state.status_popup.snapshot = Some(snapshot(
        vec![segment(ContextCategory::UserMessages, 100)],
        1000,
    ));
    state.conversation.chat_area = Rect::new(0, 1, 79, 14);

    let text = draw(&mut state, 80, 20);

    assert_eq!(state.status_popup.popup_rect.y, 1);
    assert_eq!(state.status_popup.popup_rect.height, 14);
    let rect = state.status_popup.popup_rect;
    let scrollbar_column = (rect.x + rect.width - 3) as usize;
    let rows: Vec<Vec<char>> = text.lines().map(|row| row.chars().collect()).collect();
    for row in &rows[(rect.y + 2) as usize..(rect.y + rect.height - 2) as usize] {
        assert_eq!(row[scrollbar_column - 1], ' ', "gap before the scrollbar");
    }
}

#[test]
fn a_short_terminal_scrolls_instead_of_truncating() {
    let mut state = TuiState::new();
    state.status_popup.snapshot = Some(snapshot(
        vec![segment(ContextCategory::UserMessages, 100)],
        1000,
    ));
    state.status_popup.scroll_offset = 999;

    draw(&mut state, 80, 14);

    assert!(state.status_popup.scroll_offset > 0);
    assert!(state.status_popup.visible_rows > 0);
}

#[test]
fn history_lists_model_changes_and_compactions_in_order() {
    use crate::agent::StatusTimelineEntry::{Compaction, Model};
    let mut status = snapshot(vec![segment(ContextCategory::UserMessages, 100)], 1000);
    let parameters = |context_length| crate::store::ModelParameters {
        context_length: Some(context_length),
        reasoning_effort: Some("high".to_string()),
        ..Default::default()
    };
    status.timeline = vec![
        Model {
            at: "2026-01-01T10:00:00.000Z".to_string(),
            provider_connection: "mock".to_string(),
            model: "a".to_string(),
            parameters: parameters(128_000),
        },
        Compaction {
            at: "2026-01-01T10:30:00.000Z".to_string(),
            effect: Some(crate::store::CompactionEffect {
                estimated_tokens_before: 800,
                estimated_tokens_after: 200,
            }),
        },
        Compaction {
            at: String::new(),
            effect: Some(crate::store::CompactionEffect {
                estimated_tokens_before: 400,
                estimated_tokens_after: 300,
            }),
        },
        Compaction {
            at: String::new(),
            effect: None,
        },
        Model {
            at: "2026-01-01T11:00:00.000Z".to_string(),
            provider_connection: "mock".to_string(),
            model: "b".to_string(),
            parameters: parameters(32_000),
        },
    ];
    status.finished_subagent = true;
    let lines = texts(&status, 80);
    let joined = lines.join("\n");
    assert!(
        joined.contains("2026-01-01 10:00Z  model changed: mock/a"),
        "{joined}"
    );
    assert!(joined.contains("effort high"), "{joined}");
    assert!(joined.contains("compaction #1"), "{joined}");
    assert!(joined.contains("800 → 200 tokens (-75.0%)"), "{joined}");
    assert!(joined.contains("effect not recorded"), "{joined}");
    assert!(!joined.contains("Reduction"), "{joined}");
    assert!(joined.contains("compaction #3"), "{joined}");
    assert!(joined.contains("Average reduction"), "{}", joined);
    assert!(joined.contains("were not recorded"), "{joined}");
    let second_model = lines
        .iter()
        .find(|line| line.contains("mock/b"))
        .expect("second model row");
    assert!(
        second_model.contains("32K window") || second_model.contains("32K window"),
        "{second_model}"
    );
    assert!(
        !second_model.contains("effort"),
        "unchanged parameters are omitted: {second_model}"
    );
}

#[test]
fn history_precedes_context_usage() {
    let status = snapshot(vec![segment(ContextCategory::UserMessages, 100)], 1000);
    let lines = texts(&status, 80);
    let position = |needle: &str| lines.iter().position(|line| line.starts_with(needle));
    assert!(position("History") < position("Context usage"));
    assert!(position("History") > position("Current setup"));
}

#[test]
fn narrow_table_drops_the_window_column_and_keeps_the_used_column() {
    let status = snapshot(vec![segment(ContextCategory::UserMessages, 100)], 1000);
    let lines = texts(&status, 40);
    let header = lines
        .iter()
        .find(|line| line.contains("Contribution"))
        .expect("table header");
    assert!(
        header.contains("% used") && header.contains("Tokens"),
        "{header}"
    );
    assert!(!header.contains("% window"), "{header}");
}

#[test]
fn context_usage_title_reports_the_free_share() {
    let status = snapshot(vec![segment(ContextCategory::UserMessages, 100)], 1000);
    let lines = texts(&status, 80);
    assert!(
        lines
            .iter()
            .any(|line| line.contains("10.0% of window, 90.0% free")),
        "{lines:?}"
    );
}

#[test]
fn tools_show_call_counts_only_for_used_tools() {
    let mut status = snapshot(vec![segment(ContextCategory::UserMessages, 100)], 1000);
    status.tool_call_counts.insert("read".to_string(), 12);
    status.tool_call_counts.insert("exec".to_string(), 1);
    let joined = texts(&status, 80).join("\n");
    assert!(joined.contains("read (12), write"), "{joined}");
    assert!(!joined.contains("exec"), "{joined}");
}

#[test]
fn tools_are_sorted_by_call_count_then_name() {
    let mut status = snapshot(vec![segment(ContextCategory::UserMessages, 100)], 1000);
    status.allowed_tools = ["write", "grep", "read", "edit", "bash"]
        .map(String::from)
        .to_vec();
    status.tool_call_counts.insert("write".to_string(), 3);
    status.tool_call_counts.insert("read".to_string(), 3);
    status.tool_call_counts.insert("bash".to_string(), 7);
    let joined = texts(&status, 80).join("\n");
    assert!(
        joined.contains("bash (7), read (3), write (3), edit, grep"),
        "{joined}"
    );
}

#[test]
fn the_finished_subagent_note_wraps_instead_of_truncating() {
    let mut status = snapshot(vec![segment(ContextCategory::UserMessages, 100)], 1000);
    status.finished_subagent = true;
    let lines = texts(&status, 40);
    let start = lines
        .iter()
        .position(|line| line.starts_with("Finished subagent"))
        .expect("note present");
    let note = lines[start..].join(" ");
    assert!(note.contains("were not recorded."), "{note}");
    assert!(lines[start..].iter().all(|line| line.chars().count() <= 40));
}

#[test]
fn long_history_lines_wrap_under_the_event_text_instead_of_truncating() {
    use crate::agent::StatusTimelineEntry::Model;
    let mut status = snapshot(vec![segment(ContextCategory::UserMessages, 100)], 1000);
    status.timeline = vec![Model {
        at: "2026-01-01T10:00:00.000Z".to_string(),
        provider_connection: "openrouter".to_string(),
        model: "openai/gpt-6-luna".to_string(),
        parameters: crate::store::ModelParameters {
            context_length: Some(256_000),
            reasoning_effort: Some("medium".to_string()),
            max_output_tokens: Some(32_000),
            ..Default::default()
        },
    }];
    let lines = texts(&status, 50);
    let start = lines
        .iter()
        .position(|line| line.starts_with("2026-01-01 10:00Z  model changed:"))
        .expect("history line");
    assert!(lines[start..=start + 1]
        .iter()
        .all(|line| line.chars().count() <= 50));
    let joined = lines[start..].join(" ");
    assert!(joined.contains("max out"), "{joined}");
    assert!(
        lines[start + 1].starts_with("                   "),
        "{lines:?}"
    );
}
