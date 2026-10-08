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

use crate::events::AppAction;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen, SetTitle,
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io::{self, Write};
use std::time::Duration;
use tokio::sync::mpsc;

/// Abstracts writing text to the OS clipboard so the Driver can be tested
/// without side-effects on the host machine.
pub trait ClipboardSink: Send {
    /// Copy `text` to the clipboard. Returns `true` on success.
    fn write_text(&mut self, text: String) -> bool;
}

/// Production implementation that delegates to [`arboard`].
pub struct ArboardClipboard;

impl ClipboardSink for ArboardClipboard {
    fn write_text(&mut self, text: String) -> bool {
        match arboard::Clipboard::new() {
            Ok(mut clipboard) => clipboard.set_text(text).is_ok(),
            Err(error) => {
                tracing::warn!(?error, "clipboard unavailable");
                false
            }
        }
    }
}

/// No-op clipboard used in tests so that no OS clipboard is touched.
pub struct NoopClipboard;

impl ClipboardSink for NoopClipboard {
    fn write_text(&mut self, _text: String) -> bool {
        true
    }
}

const DEFAULT_TERMINAL_TITLE: &str = "Ylva Agent Harness";

/// Format the terminal title without allowing model output to inject terminal
/// control characters into the title escape sequence.
pub(super) fn format_terminal_title(session_title: Option<&str>) -> String {
    let sanitized_title = session_title
        .unwrap_or_default()
        .chars()
        .filter(|character| !character.is_control())
        .collect::<String>()
        .trim()
        .to_string();
    if sanitized_title.is_empty() {
        DEFAULT_TERMINAL_TITLE.to_string()
    } else {
        format!("{sanitized_title} - {DEFAULT_TERMINAL_TITLE}")
    }
}

pub(super) fn set_terminal_title(session_title: Option<&str>) -> io::Result<()> {
    let mut stdout = io::stdout();
    execute!(stdout, SetTitle(format_terminal_title(session_title)))
}

/// Saves and restores the title around the TUI session where the terminal
/// supports the standard xterm title stack controls.
pub(super) struct TerminalTitleGuard;

impl TerminalTitleGuard {
    pub(super) fn new() -> io::Result<Self> {
        let mut stdout = io::stdout();
        write!(stdout, "\x1b[22;0t")?;
        stdout.flush()?;
        Ok(Self)
    }
}

impl Drop for TerminalTitleGuard {
    fn drop(&mut self) {
        let result = (|| -> io::Result<()> {
            let mut stdout = io::stdout();
            write!(stdout, "\x1b[23;0t")?;
            stdout.flush()
        })();
        if let Err(error) = result {
            tracing::debug!(%error, "could not restore the terminal title");
        }
    }
}

/// RAII guard restoring the terminal to its pre-TUI state.
pub(super) struct TerminalGuard;

pub(super) fn initialize_terminal(
) -> io::Result<(TerminalGuard, Terminal<CrosstermBackend<io::Stdout>>)> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        Hide,
        EnableMouseCapture,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
        EnableBracketedPaste,
        SetTitle(format_terminal_title(None)),
    )?;
    let guard = TerminalGuard;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok((guard, terminal))
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            Show,
            DisableMouseCapture,
            PopKeyboardEnhancementFlags,
            DisableBracketedPaste,
        );
        let _ = disable_raw_mode();
    }
}

/// Bridge OS SIGINT to `AppAction::SigInt`.
pub(super) fn install_sigint_relay(action_tx: mpsc::Sender<AppAction>) -> anyhow::Result<()> {
    let (sigint_tx, sigint_rx) = std::sync::mpsc::channel::<()>();
    ctrlc::set_handler(move || {
        let _ = sigint_tx.send(());
    })?;
    tokio::task::spawn_blocking(move || {
        while !action_tx.is_closed() {
            match sigint_rx.recv_timeout(Duration::from_millis(50)) {
                Ok(()) => {
                    if action_tx.blocking_send(AppAction::SigInt).is_err() {
                        break;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });
    Ok(())
}
