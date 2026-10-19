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

//! Subagent lifecycle records and transcript persistence.

use uuid::Uuid;

use super::title::subagent_title_outcome;
use super::{now_timestamp, Store, StoreError, StoreEvent, EVENT_VERSION};
use crate::agent::Message;
use crate::subagent_manager::FinishResultOrigin;

impl Store {
    /// Enqueue the `subagent_start` record written to the **parent** session
    /// before a subagent run begins.
    pub fn record_subagent_start(
        &self,
        run_id: Uuid,
        call_id: Uuid,
        agent_type: &str,
        task: &str,
        label: &str,
    ) {
        self.record_event(StoreEvent::SubagentStart {
            version: EVENT_VERSION,
            created_at: now_timestamp(),
            id: run_id,
            parent_session: self.inner.session_id,
            call_id,
            agent_type: agent_type.to_string(),
            task: task.to_string(),
            label: label.to_string(),
        });
    }

    /// Enqueue the `subagent_result` record written to the **parent** session
    /// when a subagent run completes.
    pub fn record_subagent_result(
        &self,
        run_id: Uuid,
        call_id: Uuid,
        status: &str,
        result: Option<String>,
        sources: Vec<String>,
        origin: FinishResultOrigin,
    ) {
        self.record_event(StoreEvent::SubagentResult {
            version: EVENT_VERSION,
            completed_at: now_timestamp(),
            id: run_id,
            parent_session: self.inner.session_id,
            call_id,
            status: status.to_string(),
            result,
            sources,
            origin,
        });
    }

    /// Write a completed subagent transcript **once**, into its own
    /// sub-session file under the same `project_key`, with its own `meta`
    /// header titled asynchronously from the first explicit user message and
    /// structured `agent_finish` outcome, or a complete assistant reply. Subagent
    /// transcripts are never streamed live.
    ///
    /// `provider_connection_and_model` is the subagent's own resolved engine —
    /// which can differ from the parent's. `None` (the subagent's engine
    /// configuration could not be resolved) falls back to an empty
    /// `meta.provider_connection`/`model`
    /// pair and skips the initial [`StoreEvent::ModelChanged`] record rather
    /// than persisting a misleading empty one. The supplied messages retain
    /// their order and are persisted as given.
    pub async fn write_subagent_transcript(
        &self,
        provider_connection_and_model: Option<(&str, &str)>,
        messages: &[Message],
    ) -> Result<Uuid, StoreError> {
        Ok(self
            .write_subagent_transcript_store(provider_connection_and_model, messages)
            .await?
            .session_id())
    }

    /// Write a subagent transcript and retain its store handle for follow-up
    /// metadata operations such as utility-engine title generation.
    pub async fn write_subagent_transcript_store(
        &self,
        provider_connection_and_model: Option<(&str, &str)>,
        messages: &[Message],
    ) -> Result<Store, StoreError> {
        let (provider_connection, model) = provider_connection_and_model.unwrap_or(("", ""));
        let subagent_store = self.start_new_session(
            provider_connection,
            model,
            Some(self.inner.session_id),
            None,
        )?;
        if provider_connection_and_model.is_some() {
            subagent_store.record_model_changed(provider_connection, model);
        }
        subagent_store.record_subagent_transcript_messages(messages);
        subagent_store.flush().await;
        Ok(subagent_store)
    }

    /// Write a subagent transcript using the subagent run id as its session id.
    /// Parent `subagent_start` records already persist that run id, so this stable
    /// path lets replay locate the transcript without adding another
    /// cross-file identifier to the parent event schema.
    pub async fn write_subagent_transcript_for_run(
        &self,
        run_id: Uuid,
        provider_connection_and_model: Option<(&str, &str)>,
        messages: &[Message],
    ) -> Result<Uuid, StoreError> {
        Ok(self
            .write_subagent_transcript_for_run_store(
                run_id,
                provider_connection_and_model,
                messages,
            )
            .await?
            .session_id())
    }

    /// Write a subagent transcript using the run id and retain its store handle
    /// for follow-up metadata operations.
    pub async fn write_subagent_transcript_for_run_store(
        &self,
        run_id: Uuid,
        provider_connection_and_model: Option<(&str, &str)>,
        messages: &[Message],
    ) -> Result<Store, StoreError> {
        let (provider_connection, model) = provider_connection_and_model.unwrap_or(("", ""));
        let subagent_store = self.start_new_session_with_id(
            run_id,
            provider_connection,
            model,
            Some(self.inner.session_id),
            None,
        )?;
        if provider_connection_and_model.is_some() {
            subagent_store.record_model_changed(provider_connection, model);
        }
        subagent_store.record_subagent_transcript_messages(messages);
        subagent_store.flush().await;
        Ok(subagent_store)
    }

    fn record_subagent_transcript_messages(&self, messages: &[Message]) {
        for message in messages {
            self.record_message(message);
        }
        self.lock_title_snapshot().completed_subagent_outcome = subagent_title_outcome(messages);
    }
}
