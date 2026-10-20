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

//! Session title generation triggered from the TUI event loop.

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::agent::{AgentLoop, UiUpdate};
use crate::agent_factory::AgentFactory;
use crate::config::ProfileConfig;
use crate::session_title::{SessionTitleUpdate, TitleAttempt};

pub(super) struct TitleDriver {
    pub(super) factory: Arc<AgentFactory>,
    pub(super) profile: ProfileConfig,
    pub(super) attempt: TitleAttempt,
    pub(super) title_update_tx: mpsc::UnboundedSender<SessionTitleUpdate>,
}

impl TitleDriver {
    pub(super) fn trigger(&self, agent: &AgentLoop) {
        let Some(store) = agent.store() else {
            return;
        };
        let _ = if self.profile.utility_engine.is_none() {
            // Without an explicit utility engine, titles follow the engine the
            // conversation currently runs on, including `/models` switches.
            self.attempt.trigger_with_engine_and_title_updates(
                store,
                agent.engine(),
                self.title_update_tx.clone(),
            )
        } else {
            self.attempt.trigger_with_title_updates(
                store,
                Arc::clone(&self.factory),
                self.profile.clone(),
                self.title_update_tx.clone(),
            )
        };
    }
}

pub(super) fn trigger_title_on_stream_done(
    update: &UiUpdate,
    title_driver: Option<&TitleDriver>,
    agent: &AgentLoop,
    active_title_session_id: &mut Option<uuid::Uuid>,
) {
    if !matches!(update, UiUpdate::StreamDone) {
        return;
    }
    if let Some(title_driver) = title_driver {
        *active_title_session_id = agent.store().map(|store| store.session_id());
        title_driver.trigger(agent);
    }
}
