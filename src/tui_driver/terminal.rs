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
use crate::stop_signal::StopSignals;
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

#[cfg(test)]
thread_local! {
    static RESTORE_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static GUARD_DROP_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// RAII guard restoring the terminal to its pre-TUI state.
pub(super) struct TerminalGuard;

pub(super) fn initialize_terminal(
) -> io::Result<(TerminalGuard, Terminal<CrosstermBackend<io::Stdout>>)> {
    initialize_with_guard(enable_raw_mode, || {
        install_restoring_panic_hook();
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
        Terminal::new(CrosstermBackend::new(stdout))
    })
}

/// Creates the restoring guard as soon as raw mode is enabled, so a failure in
/// any later setup step still restores the terminal.
fn initialize_with_guard<T>(
    enable_raw: impl FnOnce() -> io::Result<()>,
    setup: impl FnOnce() -> io::Result<T>,
) -> io::Result<(TerminalGuard, T)> {
    enable_raw()?;
    let guard = TerminalGuard;
    let value = setup()?;
    Ok((guard, value))
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        #[cfg(test)]
        GUARD_DROP_COUNT.with(|count| count.set(count.get() + 1));
        restore_terminal();
    }
}

/// Undo every terminal mode the TUI enabled. Idempotent and best-effort, so it
/// is safe to call from the guard, a panic hook and a signal handler alike.
fn restore_terminal() {
    #[cfg(test)]
    RESTORE_COUNT.with(|count| count.set(count.get() + 1));
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

/// Restore the terminal before the previous panic hook prints its message, so
/// the message lands on the normal screen and a panic in any thread or task
/// (which the guard never sees) cannot leave mouse capture enabled.
fn install_restoring_panic_hook() {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        restore_terminal();
        previous_hook(panic_info);
    }));
}

/// Exit code for termination by SIGHUP (128 + signal number 1).
const SIGHUP_EXIT_CODE: i32 = 129;

/// On SIGHUP restore the terminal and exit immediately. SIGHUP is not a stop
/// request, so it bypasses the graceful stop path. Must run inside a Tokio
/// runtime.
fn install_hangup_restore() -> io::Result<()> {
    let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    tokio::spawn(async move {
        if hangup.recv().await.is_some() {
            restore_terminal();
            std::process::exit(SIGHUP_EXIT_CODE);
        }
    });
    Ok(())
}

/// Bridge external SIGINT/SIGTERM to `AppAction::StopSignal`. Must run inside a Tokio runtime.
pub(super) fn install_stop_signal_relay(action_tx: mpsc::Sender<AppAction>) -> anyhow::Result<()> {
    let mut stop_signals = StopSignals::install()?;
    install_hangup_restore()?;
    tokio::spawn(async move {
        loop {
            tokio::select! {
                () = action_tx.closed() => break,
                received = stop_signals.recv() => {
                    if received.is_none() || action_tx.send(AppAction::StopSignal).await.is_err() {
                        break;
                    }
                }
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropping_the_guard_restores_the_terminal() {
        RESTORE_COUNT.with(|count| count.set(0));

        drop(TerminalGuard);

        assert_eq!(RESTORE_COUNT.with(std::cell::Cell::get), 1);
    }

    #[test]
    fn restoring_twice_does_not_fail() {
        restore_terminal();
        restore_terminal();
    }

    #[test]
    fn failed_setup_after_raw_mode_drops_the_guard() {
        GUARD_DROP_COUNT.with(|count| count.set(0));

        let result =
            initialize_with_guard(|| Ok(()), || Err::<(), _>(io::Error::other("setup failed")));

        assert!(result.is_err());
        assert_eq!(GUARD_DROP_COUNT.with(std::cell::Cell::get), 1);
    }

    #[test]
    fn failed_raw_mode_does_not_create_a_guard() {
        GUARD_DROP_COUNT.with(|count| count.set(0));

        let result = initialize_with_guard(|| Err(io::Error::other("no tty")), || Ok(()));

        assert!(result.is_err());
        assert_eq!(GUARD_DROP_COUNT.with(std::cell::Cell::get), 0);
    }
}
