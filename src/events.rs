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

//! Crossterm event bridge.
//!
//! `crossterm::event::read()` blocks, so it is pushed onto a
//! `spawn_blocking` thread and every event forwarded over a bounded `mpsc`
//! channel. The async TUI loop never blocks on terminal I/O (blueprint §4B).
//!
//! This module only **maps** raw events to [`AppAction`]s. All context-dependent
//! interpretation (what `Esc` means, double-press confirm state, tool-approval
//! sub-mode) lives in the main task, not here.

use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// A normalized user input routed from the crossterm bridge into the main TUI
/// loop, where context decides what it means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppAction {
    /// Ctrl+C pressed: stop if busy; quit if idle (after double-press confirm).
    CtrlC,
    /// Esc pressed: mode-dependent — see `run_tui` dispatch table.
    Escape,
    /// F1 pressed — toggle Help popup.
    ShowHelp,
    /// OS SIGINT (external, e.g. `kill -INT`): interrupt immediately, no
    /// double-press. Distinct from `CtrlC`, which is a terminal key event that
    /// follows the double-press confirm.
    SigInt,
    /// All other key events forwarded to the main task.
    KeyInput(KeyEvent),
    /// Mouse events forwarded to the main task.
    MouseInput(crossterm::event::MouseEvent),
    /// Terminal resize.
    Resize { width: u16, height: u16 },
    /// Bracketed paste event from the terminal.
    Paste(String),
}

/// Map a single crossterm `Event` to an [`AppAction`].
///
/// Only `KeyEventKind::Press` events are considered; release/repeat are
/// ignored. `Ctrl+C`, `Esc`, and `F1` are lifted into dedicated variants so
/// the main task can branch on intent rather than raw key codes. Terminal
/// encodings of modified Enter are normalized before other key events are
/// forwarded.
pub fn map_event(event: Event) -> Option<AppAction> {
    match event {
        Event::Key(key) => map_key_event(key),
        Event::Mouse(mouse) => Some(AppAction::MouseInput(mouse)),
        Event::Resize(width, height) => Some(AppAction::Resize { width, height }),
        Event::Paste(text) => Some(AppAction::Paste(text)),
        _ => None,
    }
}

fn map_key_event(key: KeyEvent) -> Option<AppAction> {
    if key.kind != KeyEventKind::Press {
        return None;
    }
    let key = normalize_modified_enter(key);
    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(AppAction::CtrlC)
        }
        KeyCode::Esc => Some(AppAction::Escape),
        KeyCode::F(1) => Some(AppAction::ShowHelp),
        _ => Some(AppAction::KeyInput(key)),
    }
}

fn normalize_modified_enter(mut key: KeyEvent) -> KeyEvent {
    if key.code == KeyCode::Char('j') && key.modifiers == KeyModifiers::CONTROL {
        key.code = KeyCode::Enter;
        key.modifiers = KeyModifiers::SHIFT;
    } else if key.code == KeyCode::Char('\n') && key.modifiers.contains(KeyModifiers::SHIFT) {
        key.code = KeyCode::Enter;
    }
    key
}

/// Spawn a blocking thread that reads `crossterm::event::read()` in a tight
/// loop and forwards every mapped [`AppAction`] to `action_tx`.
///
/// The thread exits when `action_tx` is dropped (channel closed) or the
/// terminal read fails. The returned `JoinHandle` lets the caller await the
/// bridge during shutdown.
///
/// A plain blocking `read()` would hold the thread forever and hang the Tokio
/// runtime on shutdown (blocking threads are not interrupted), so the loop
/// `poll`s with a short timeout and observes channel closure, exiting promptly
/// once the consumer is gone.
pub fn spawn_crossterm_bridge(action_tx: mpsc::Sender<AppAction>) -> JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        run_bridge_loop(&action_tx, crossterm::event::poll, crossterm::event::read);
    })
}

/// The crossterm bridge loop, parameterized over the blocking `poll`/`read`
/// calls so its behaviour can be unit-tested without a live terminal.
///
/// Loops until the action channel is closed, then exits so the blocking thread
/// is never stranded on a terminal read during shutdown.
fn run_bridge_loop<FP, FR>(action_tx: &mpsc::Sender<AppAction>, poll: FP, read: FR)
where
    FP: Fn(Duration) -> std::io::Result<bool>,
    FR: Fn() -> std::io::Result<Event>,
{
    while !action_tx.is_closed() {
        if poll(Duration::from_millis(50)).unwrap_or(false) {
            if let Ok(event) = read() {
                if let Some(action) = map_event(event) {
                    if action_tx.blocking_send(action).is_err() {
                        break;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, MouseEvent, MouseEventKind};

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn ctrl_c_maps_to_ctrl_c_action() {
        let action = map_event(Event::Key(press(KeyCode::Char('c'), KeyModifiers::CONTROL)));
        assert_eq!(action, Some(AppAction::CtrlC));
    }

    #[test]
    fn escape_maps_to_escape_action() {
        let action = map_event(Event::Key(press(KeyCode::Esc, KeyModifiers::NONE)));
        assert_eq!(action, Some(AppAction::Escape));
    }

    #[test]
    fn f1_maps_to_show_help_action() {
        let action = map_event(Event::Key(press(KeyCode::F(1), KeyModifiers::NONE)));
        assert_eq!(action, Some(AppAction::ShowHelp));
    }

    #[test]
    fn regular_character_maps_to_key_input() {
        let key = press(KeyCode::Char('a'), KeyModifiers::NONE);
        let action = map_event(Event::Key(key));
        assert_eq!(action, Some(AppAction::KeyInput(key)));
    }

    #[test]
    fn line_feed_from_shift_enter_maps_to_a_modified_enter() {
        // Zed's terminal encodes Shift+Enter as LF. In crossterm raw mode,
        // LF is decoded as Ctrl+J, so normalize that terminal-specific representation
        // before context-dependent key routing.
        for key in [
            press(KeyCode::Char('j'), KeyModifiers::CONTROL),
            press(KeyCode::Char('\n'), KeyModifiers::SHIFT),
        ] {
            assert_eq!(
                map_event(Event::Key(key)),
                Some(AppAction::KeyInput(press(
                    KeyCode::Enter,
                    KeyModifiers::SHIFT
                )))
            );
        }
    }

    #[test]
    fn uppercase_character_is_not_swallowed_by_ctrl_c() {
        // Ctrl+Shift+C (uppercase 'C') is not Ctrl+C.
        let key = press(KeyCode::Char('C'), KeyModifiers::SHIFT);
        let action = map_event(Event::Key(key));
        assert_eq!(action, Some(AppAction::KeyInput(key)));
    }

    #[test]
    fn key_release_is_ignored() {
        let mut key = press(KeyCode::Char('a'), KeyModifiers::NONE);
        key.kind = KeyEventKind::Release;
        assert_eq!(map_event(Event::Key(key)), None);
    }

    #[test]
    fn mouse_event_maps_to_mouse_input() {
        let mouse = MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 1,
            row: 2,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            map_event(Event::Mouse(mouse)),
            Some(AppAction::MouseInput(mouse))
        );
    }

    #[test]
    fn resize_event_maps_to_resize_action() {
        assert_eq!(
            map_event(Event::Resize(120, 40)),
            Some(AppAction::Resize {
                width: 120,
                height: 40
            })
        );
    }

    #[test]
    fn paste_event_maps_to_paste_action() {
        let action = map_event(Event::Paste("hello\nworld".to_string()));
        assert_eq!(action, Some(AppAction::Paste("hello\nworld".to_string())));
    }

    #[test]
    fn bridge_loop_forwards_events_until_channel_closes() {
        let (action_tx, mut action_rx) = mpsc::channel(4);
        let sent = Event::Key(press(KeyCode::Char('a'), KeyModifiers::NONE));
        let handle = std::thread::spawn(move || {
            // Poll always reports an event ready; read returns the key each time.
            run_bridge_loop(&action_tx, |_| Ok(true), || Ok(sent.clone()));
        });

        let got = action_rx
            .blocking_recv()
            .expect("forwarded action is received");
        assert_eq!(
            got,
            AppAction::KeyInput(press(KeyCode::Char('a'), KeyModifiers::NONE))
        );

        // Closing the receiver makes the loop observe channel closure and exit.
        drop(action_rx);
        handle.join().expect("bridge loop exits on channel close");
    }
}
