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

//! Headless CLI output: stream sections, ANSI styling and section flushing.

use std::io::Write;

use ratatui::style::Color;

use crate::theme::thinking_border_color;

/// Which kind of assistant output is currently being written to the CLI
/// stream, mirroring the TUI's `partial_thinking_text` /
/// `partial_assistant_text` split (see `AppState`). The CLI has no bubbles to
/// flush into, so a section switch is rendered as a header/footer line
/// instead, but the switching rules are the same: a response chunk closes an
/// open thinking segment and vice versa, and a tool call or stream end closes
/// whatever segment is open.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum StreamSection {
    None,
    Thinking,
    Response,
}

/// How long a tool runs before the CLI prints a "running…" line for it.
const DEFAULT_SLOW_TOOL_NOTICE_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

/// Presentation options for the headless CLI output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CliOutputOptions {
    color: bool,
    slow_tool_notice_delay: std::time::Duration,
}

impl CliOutputOptions {
    /// Create output options with or without ANSI colors.
    pub const fn new(color: bool) -> Self {
        Self {
            color,
            slow_tool_notice_delay: DEFAULT_SLOW_TOOL_NOTICE_DELAY,
        }
    }

    /// Set how long a tool must run before a "running…" line is printed for it.
    pub const fn with_slow_tool_notice_delay(mut self, delay: std::time::Duration) -> Self {
        self.slow_tool_notice_delay = delay;
        self
    }

    pub(super) const fn slow_tool_notice_delay(self) -> std::time::Duration {
        self.slow_tool_notice_delay
    }

    /// Disable ANSI colors. This is the default for the explicit-writer
    /// helper so tests and callers that capture output remain plain.
    pub const fn plain() -> Self {
        Self::new(false)
    }

    pub(super) const fn color_enabled(self) -> bool {
        self.color
    }
}

pub(super) fn ansi_foreground_code(color: Color) -> String {
    match color {
        Color::Reset => "39".to_string(),
        Color::Black => "30".to_string(),
        Color::Red => "31".to_string(),
        Color::Green => "32".to_string(),
        Color::Yellow => "33".to_string(),
        Color::Blue => "34".to_string(),
        Color::Magenta => "35".to_string(),
        Color::Cyan => "36".to_string(),
        Color::Gray => "37".to_string(),
        Color::DarkGray => "90".to_string(),
        Color::LightRed => "91".to_string(),
        Color::LightGreen => "92".to_string(),
        Color::LightYellow => "93".to_string(),
        Color::LightBlue => "94".to_string(),
        Color::LightMagenta => "95".to_string(),
        Color::LightCyan => "96".to_string(),
        Color::White => "97".to_string(),
        Color::Rgb(red, green, blue) => format!("38;2;{red};{green};{blue}"),
        Color::Indexed(index) => format!("38;5;{index}"),
    }
}

pub(super) fn write_cli_styled_line(
    out: &mut (dyn Write + Send),
    text: &str,
    color: Color,
    dim: bool,
    options: CliOutputOptions,
) -> anyhow::Result<()> {
    if !options.color_enabled() {
        writeln!(out, "{text}")?;
        return Ok(());
    }

    let foreground_code = ansi_foreground_code(color);
    if dim {
        writeln!(out, "\x1b[2;{foreground_code}m{text}\x1b[0m")?;
    } else {
        writeln!(out, "\x1b[{foreground_code}m{text}\x1b[0m")?;
    }
    Ok(())
}

/// Close the current assistant output section. Thinking sections get an
/// elapsed-duration footer; response sections get their terminating newline.
/// Leaves `current_section` at `None` so callers can open the next section.
pub(super) fn flush_stream_section(
    out: &mut (dyn Write + Send),
    current_section: &mut StreamSection,
    thinking_started_at: &mut Option<std::time::Instant>,
    output_options: CliOutputOptions,
) -> anyhow::Result<()> {
    match *current_section {
        StreamSection::Thinking => {
            let elapsed = thinking_started_at
                .take()
                .map(|started_at| started_at.elapsed())
                .unwrap_or_default();
            writeln!(out)?;
            write_cli_styled_line(
                out,
                &format!("[thought for {:.3}s]", elapsed.as_secs_f64()),
                thinking_border_color(),
                true,
                output_options,
            )?;
        }
        StreamSection::Response => {
            writeln!(out)?;
        }
        StreamSection::None => {}
    }
    *current_section = StreamSection::None;
    Ok(())
}
