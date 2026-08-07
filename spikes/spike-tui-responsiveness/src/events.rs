use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::sync::mpsc;

use crate::app::AgentCommand;

/// SPIKE_NOTE: A helper bridging crossterm input into the async TUI loop.
///
/// `read()` is pushed onto a `spawn_blocking` thread (see `main.rs`) and each
/// event forwarded over an `mpsc` channel, so the async loop never blocks on
/// terminal I/O.
///
/// Translates a crossterm event into an [`AgentCommand`] when applicable, echoing
/// printable keys / backspace into `input_buffer` and returning `None`.
/// `Enter` drains the input buffer into [`AgentCommand::Prompt`].
pub fn handle_input_event(event: Event, input_buffer: &mut String) -> Option<AgentCommand> {
    let Event::Key(key) = event else {
        return None;
    };
    if key.kind != KeyEventKind::Press {
        return None;
    }

    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(AgentCommand::Stop)
        }
        KeyCode::Enter => {
            let text = std::mem::take(input_buffer);
            Some(AgentCommand::Prompt(text))
        }
        KeyCode::Char(ch) => {
            if !key.modifiers.contains(KeyModifiers::CONTROL) {
                input_buffer.push(ch);
            }
            None
        }
        KeyCode::Backspace => {
            input_buffer.pop();
            None
        }
        _ => None,
    }
}

/// Returns `true` for an unmodified `q` key press so the operator can exit
/// early without disturbing the input buffer (the run otherwise auto-exits at
/// `--duration`).
pub fn is_quit_event(event: &Event) -> bool {
    matches!(
        event,
        Event::Key(KeyEvent {
            code: KeyCode::Char('q'),
            kind: KeyEventKind::Press,
            ..
        })
    )
}

/// Spawn a blocking thread that forwards `crossterm::event::read()` results into
/// the returned async channel receiver.
pub fn spawn_event_pump() -> mpsc::Receiver<Event> {
    let (event_tx, event_rx) = mpsc::channel::<Event>(32);
    tokio::task::spawn_blocking(move || {
        loop {
            if let Ok(event) = crossterm::event::read()
                && event_tx.blocking_send(event).is_err()
            {
                break;
            }
        }
    });
    event_rx
}
