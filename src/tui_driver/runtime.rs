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

//! Runtime inputs handed from [`TuiApp`](super::TuiApp) to the event loop.

use std::sync::Arc;

use tokio::sync::mpsc;

use super::favorites::FavoriteSaveOwner;
use super::title::TitleDriver;
use crate::config::ProfileConfig;
use crate::session_title::SessionTitleUpdate;
use crate::tui_reducer_state::TuiDisplayMetadata;

pub(super) struct TuiRuntime {
    pub(super) driver: Option<TitleDriver>,
    pub(super) update_rx: mpsc::UnboundedReceiver<SessionTitleUpdate>,
    pub(super) display_metadata: TuiDisplayMetadata,
    pub(super) input_history_limit: usize,
    pub(super) exit_snapshot: Option<crate::app::SessionExitHandle>,
    pub(super) unactivated_clones: Arc<std::sync::Mutex<Vec<uuid::Uuid>>>,
    /// Shown once as a conversation notice when the TUI starts.
    pub(super) startup_notice: Option<String>,
    /// Favorite-model persistence; lives for the whole TUI run, outside every
    /// resettable conversation state.
    pub(super) favorite_owner: FavoriteSaveOwner,
    /// Outcome of a favorite save that finished during shutdown, printed by
    /// the app after the terminal is restored.
    pub(super) favorite_shutdown_reports: Arc<std::sync::Mutex<Vec<String>>>,
}

impl TuiRuntime {
    pub(super) fn disabled() -> Self {
        let (_update_tx, update_rx) = mpsc::unbounded_channel();
        Self {
            driver: None,
            update_rx,
            display_metadata: TuiDisplayMetadata::default(),
            input_history_limit: ProfileConfig::default().input_history_limit,
            exit_snapshot: None,
            unactivated_clones: Arc::default(),
            startup_notice: None,
            favorite_owner: FavoriteSaveOwner::default(),
            favorite_shutdown_reports: Arc::default(),
        }
    }
}
