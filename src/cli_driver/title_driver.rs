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

//! Session title generation driven alongside a headless run.

use std::sync::Arc;

use crate::agent::AgentLoop;
use crate::agent_factory::AgentFactory;
use crate::config::ProfileConfig;
use crate::session_title::TitleAttempt;

pub(super) struct TitleDriver {
    pub(super) factory: Arc<AgentFactory>,
    pub(super) profile: ProfileConfig,
    pub(super) attempt: TitleAttempt,
    pub(super) pending_tasks: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl TitleDriver {
    pub(super) fn trigger(&self, agent: &AgentLoop) {
        if let Some(store) = agent.store() {
            if let Some(task) =
                self.attempt
                    .trigger(store, Arc::clone(&self.factory), self.profile.clone())
            {
                self.pending_tasks
                    .lock()
                    .expect("title task mutex is not poisoned")
                    .push(task);
            }
        }
    }

    pub(super) async fn wait_for_pending_tasks(&self) {
        let pending_tasks = std::mem::take(
            &mut *self
                .pending_tasks
                .lock()
                .expect("title task mutex is not poisoned"),
        );
        for task in pending_tasks {
            if let Err(error) = task.await {
                tracing::debug!(%error, "session title task ended unexpectedly");
            }
        }
    }
}
