use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::App;

/// Renders the scrolling token buffer plus a status line summarizing the live
/// success criteria (frame time, frames > 17 ms, streaming state, queue depth,
/// peak buffered memory, last keys).
pub fn draw(frame: &mut Frame, app: &App, duration_secs: u64) {
    let [status_area, stream_area, input_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Min(0),
            Constraint::Length(3),
        ])
        .areas(frame.area());

    draw_status(frame, status_area, app, duration_secs);
    draw_stream(frame, stream_area, app);
    draw_input(frame, input_area, app);
}

fn draw_status(frame: &mut Frame, area: Rect, app: &App, duration_secs: u64) {
    let [metric_area, key_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Length(2)])
        .areas(area);

    let state = if app.streaming { "STREAMING" } else { "idle" };
    let histogram = app.frame_time.histogram();
    let frame_text = Line::from(vec![
        Span::raw(format!(
            "frame: min {:>4.1}ms avg {:>4.1}ms max {:>4.1}ms | >17ms: {} | {} frames",
            app.frame_time.min_millis().unwrap_or(0.0),
            app.frame_time.average_millis(),
            app.frame_time.max_millis().unwrap_or(0.0),
            app.frame_time.frames_over_17ms(),
            app.frame_time.total_frames(),
        )),
        Span::styled(
            format!(
                " | hist [<8 {}|8-16 {}|16-33 {}|>33 {}]",
                histogram[0], histogram[1], histogram[2], histogram[3]
            ),
            Style::default().fg(Color::Cyan),
        ),
    ]);
    let queue_text = Line::from(vec![
        Span::styled(
            format!(
                "state: {} | queue depth: {} (peak {}) | peak buffer: {} B | soak {}s",
                state,
                app.current_queue_depth,
                app.peak_queue_depth,
                app.ring.peak_bytes(),
                duration_secs,
            ),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " | Steer: Enter  Stop: Ctrl+C  Quit: q",
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    let status = Paragraph::new(vec![frame_text, queue_text])
        .block(Block::default().borders(Borders::ALL).title("Status"));
    frame.render_widget(status, metric_area);

    let keys: Vec<Span> = {
        let mut spans = Vec::with_capacity(app.last_keys.len());
        for key in app.last_keys.iter() {
            spans.push(Span::raw(key.clone()));
            spans.push(Span::raw(" "));
        }
        spans
    };
    let key_line = if keys.is_empty() {
        Line::raw("(no keys yet)")
    } else {
        Line::from(keys)
    };
    let key_view =
        Paragraph::new(key_line).block(Block::default().borders(Borders::ALL).title("Keys"));
    frame.render_widget(key_view, key_area);
}

fn draw_stream(frame: &mut Frame, area: Rect, app: &App) {
    let lines: Vec<Line> = app
        .ring
        .iter()
        .map(|entry| Line::raw(entry.as_str()))
        .collect();
    let stream = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title("Stream"))
        .scroll((0, 0));
    frame.render_widget(stream, area);
}

fn draw_input(frame: &mut Frame, area: Rect, app: &App) {
    let input = Paragraph::new(Line::from(vec![
        Span::raw("> "),
        Span::raw(app.input.as_str()),
        Span::styled("▏", Style::default().fg(Color::Green)),
    ]))
    .block(Block::default().borders(Borders::ALL).title("Input"));
    frame.render_widget(input, area);
}
