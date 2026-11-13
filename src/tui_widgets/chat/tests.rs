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
fn scrollbar_thumb_sits_at_bottom_at_max_offset() {
    let total = 30usize;
    let visible = 10usize;
    let track_height = visible;
    let max_offset = total - visible;
    let (thumb_top, thumb_size) =
        compute_scrollbar_thumb(track_height, total, visible, max_offset, max_offset);
    assert_eq!(
        thumb_top + thumb_size,
        track_height,
        "thumb bottom must reach the last track row at max offset"
    );
}

#[test]
fn scrollbar_thumb_sits_at_top_at_zero_offset() {
    let total = 30usize;
    let visible = 10usize;
    let track_height = visible;
    let max_offset = total - visible;
    let (thumb_top, _thumb_size) =
        compute_scrollbar_thumb(track_height, total, visible, 0, max_offset);
    assert_eq!(thumb_top, 0, "thumb must start at row 0 at zero offset");
}

#[test]
fn scrollbar_thumb_never_overflows_track() {
    let total = 30usize;
    let visible = 10usize;
    let track_height = visible;
    let max_offset = total - visible;
    for offset in 0..=max_offset {
        let (thumb_top, thumb_size) =
            compute_scrollbar_thumb(track_height, total, visible, offset, max_offset);
        assert!(
            thumb_top + thumb_size <= track_height,
            "thumb overflows at offset {offset}: top={thumb_top} size={thumb_size}"
        );
    }
}

#[test]
fn scrollbar_keeps_arrows_at_two_rows() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut terminal = Terminal::new(TestBackend::new(1, 2)).expect("terminal");
    terminal
        .draw(|frame| {
            render_chat_scrollbar(frame, frame.area(), 0, 10, 12, 2, Color::Gray);
        })
        .expect("draw");
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(0, 0)].symbol(), "↑");
    assert_eq!(buffer[(0, 1)].symbol(), "↓");
}

#[test]
fn pending_model_notice_renders_before_queued_steer() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut state = TuiState::new();
    state.queued_steer_texts = vec!["queued message".to_string()];
    state.pending_model_notices = vec!["model changed notice".to_string()];
    let mut terminal = Terminal::new(TestBackend::new(60, 20)).expect("terminal");
    terminal
        .draw(|frame| draw_chat(frame, frame.area(), &mut state, &ToolRegistry::new()))
        .expect("draw");
    let lines = &state.conversation.chat_rendered_lines;
    let position = |needle: &str| lines.iter().position(|line| line.contains(needle));
    let notice_row = position("model changed notice").expect("notice rendered");
    let steer_row = position("queued message").expect("steer rendered");
    assert!(notice_row < steer_row, "{lines:?}");
}
