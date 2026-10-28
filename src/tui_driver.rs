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

//! TUI event loop — Driver layer of the reducer pattern.
//!
//! `run_tui` owns all I/O: it receives [`AppAction`]s from the crossterm
//! bridge, calls `presenter::update` (pure), then executes the returned
//! [`Effect`]s via `execute_effects`. Domain updates from [`AgentLoop`] are
//! converted to [`DomainEvent`]s and routed through the same path.

use std::time::Duration;

use ratatui::backend::Backend;
use ratatui::Terminal;
use tokio::sync::mpsc;

use crate::agent::{AgentLoop, UiUpdate};
use crate::events::AppAction;
use crate::tui_render_state::TuiState;

mod app;
mod effects;
mod event_loop;
mod favorites;
mod frame;
mod input;
mod models;
mod runtime;
mod sessions;
mod terminal;
mod timers;
mod title;
mod updates;

pub use app::TuiApp;
use event_loop::run_tui_with_title;
pub use favorites::FavoriteSaveSetup;
use runtime::TuiRuntime;

pub use terminal::{ArboardClipboard, ClipboardSink, NoopClipboard};

/// Outcome of processing one event or effect batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionOutcome {
    Continue,
    Quit,
}

/// Interval between spinner frame advances while the agent is busy.
const SPINNER_TICK_INTERVAL: Duration = Duration::from_millis(150);

/// Capacity of the bounded action channel between the crossterm bridge and the
/// async loop, and the per-frame `UiUpdate` drain cap.
const CHANNEL_CAPACITY: usize = 64;

/// Run the TUI event loop against an existing terminal.
///
/// Uses `AppState` internally (the reducer model) and returns a `TuiState`
/// for backward compatibility with integration tests.
#[doc(hidden)]
pub async fn run_tui<B>(
    terminal: &mut Terminal<B>,
    agent: AgentLoop,
    ui_rx: mpsc::Receiver<UiUpdate>,
    initial_prompt: Option<String>,
    action_rx: mpsc::Receiver<AppAction>,
    clipboard: &mut dyn ClipboardSink,
) -> anyhow::Result<TuiState>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let tui_runtime = TuiRuntime::disabled();
    run_tui_with_title(
        terminal,
        agent,
        ui_rx,
        initial_prompt,
        action_rx,
        clipboard,
        tui_runtime,
    )
    .await
    .map(|(tui_state, _outcome)| tui_state)
}

#[cfg(test)]
mod tests;
