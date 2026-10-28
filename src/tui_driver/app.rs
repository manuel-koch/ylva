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

//! Top-level TUI application entry point.

use std::sync::Arc;

use tokio::sync::mpsc;

use super::event_loop::run_tui_with_title;
use super::favorites::{FavoriteSaveOwner, FavoriteSaveSetup};
use super::runtime::TuiRuntime;
use super::terminal::{
    initialize_terminal, install_stop_signal_relay, set_terminal_title, TerminalTitleGuard,
};
use super::title::TitleDriver;
use super::{ArboardClipboard, CHANNEL_CAPACITY};
use crate::agent::Role;
use crate::agent_factory::AgentFactory;
use crate::app::App;
use crate::config::ProfileConfig;
use crate::events::{spawn_crossterm_bridge, AppAction};
use crate::exit_code::ExitCode;
use crate::session_title::TitleAttempt;
use crate::tui_reducer_state::TuiDisplayMetadata;

/// Top-level TUI application entry point.
///
/// Builds a real `CrosstermBackend` terminal, enables raw mode + alternate
/// screen, installs the SIGINT handler, constructs an [`AgentLoop`] via
/// `AgentFactory`, and guarantees the terminal is restored on every exit path.
pub struct TuiApp {
    factory: Arc<AgentFactory>,
    profile: ProfileConfig,
    agent_type: String,
    explicit_prompt: Option<String>,
    display_metadata: TuiDisplayMetadata,
    exit_snapshot: Option<crate::app::SessionExitHandle>,
    favorite_save_setup: FavoriteSaveSetup,
}

impl TuiApp {
    pub fn new(
        factory: Arc<AgentFactory>,
        profile: ProfileConfig,
        agent_type: String,
        explicit_prompt: Option<String>,
    ) -> Self {
        TuiApp {
            factory,
            profile,
            agent_type,
            explicit_prompt,
            display_metadata: TuiDisplayMetadata::default(),
            exit_snapshot: None,
            favorite_save_setup: FavoriteSaveSetup::default(),
        }
    }

    /// Set the active profile and workspace paths shown by the TUI.
    pub fn with_display_metadata(mut self, display_metadata: TuiDisplayMetadata) -> Self {
        self.display_metadata = display_metadata;
        self
    }

    /// Provide the favorite-persistence capability captured at startup from
    /// the loaded profile document. Without it, saving favorites is disabled.
    pub fn with_favorite_save_setup(mut self, favorite_save_setup: FavoriteSaveSetup) -> Self {
        self.favorite_save_setup = favorite_save_setup;
        self
    }

    pub fn with_exit_snapshot(mut self, snapshot: crate::app::SessionExitHandle) -> Self {
        self.exit_snapshot = Some(snapshot);
        self
    }
}

impl App for TuiApp {
    async fn run(self) -> anyhow::Result<u8> {
        let _terminal_title_guard = TerminalTitleGuard::new()?;
        let (_guard, mut terminal) = initialize_terminal()?;

        let (action_tx, action_rx) = mpsc::channel::<AppAction>(CHANNEL_CAPACITY);
        spawn_crossterm_bridge(action_tx.clone());
        install_stop_signal_relay(action_tx)?;

        let (agent, ui_rx) = self
            .factory
            .create(&self.profile, &self.agent_type, Role::Root)?;
        let configuration_factory = Arc::new(self.factory.without_store());
        drop(self.factory);
        let initial_session_title = agent.store().map(|store| store.title());
        set_terminal_title(initial_session_title.as_deref())?;
        let (title_update_tx, title_update_rx) = mpsc::unbounded_channel();
        let title_driver = Some(TitleDriver {
            factory: configuration_factory,
            profile: self.profile.clone(),
            attempt: TitleAttempt::default(),
            title_update_tx,
        });

        let unactivated_clones = Arc::new(std::sync::Mutex::new(Vec::new()));
        let favorite_shutdown_reports = Arc::new(std::sync::Mutex::new(Vec::new()));
        let result = run_tui_with_title(
            &mut terminal,
            agent,
            ui_rx,
            self.explicit_prompt,
            action_rx,
            &mut ArboardClipboard,
            TuiRuntime {
                driver: title_driver,
                update_rx: title_update_rx,
                display_metadata: self.display_metadata,
                input_history_limit: self.profile.input_history_limit,
                tui_config: self.profile.tui.clone(),
                exit_snapshot: self.exit_snapshot,
                unactivated_clones: unactivated_clones.clone(),
                startup_notice: self.profile.config_warnings_notice(),
                favorite_owner: FavoriteSaveOwner::from_setup(self.favorite_save_setup),
                favorite_shutdown_reports: favorite_shutdown_reports.clone(),
            },
        )
        .await;
        drop(terminal);
        drop(_guard);
        drop(_terminal_title_guard);
        for clone_id in unactivated_clones
            .lock()
            .expect("clone notice mutex")
            .iter()
        {
            eprintln!("Clone created but not activated: {clone_id}");
        }
        for report in favorite_shutdown_reports
            .lock()
            .expect("favorite report mutex")
            .iter()
        {
            eprintln!("{report}");
        }
        match result {
            Ok((_, outcome)) => Ok(u8::from(outcome.exit_code())),
            // The terminal was already up, so a failure while driving the TUI
            // is a general execution error, not a startup error (exit code 2).
            Err(error) => {
                tracing::error!(%error, "TUI run failed");
                eprintln!("error: {error}");
                Ok(u8::from(ExitCode::GeneralError))
            }
        }
    }
}
