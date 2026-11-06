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

//! `/status` for a finished subagent, derived from its persisted transcript.

use super::status_timeline::timeline_from_events;
use super::{Context, Message, StatusSnapshot};
use crate::context_breakdown::ContextBreakdown;
use crate::llm::DEFAULT_CONTEXT_LENGTH;
use crate::store::{ModelParameters, SessionState, StoreEvent};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// Final [`StatusSnapshot`]s of finished subagent runs, kept for the live
/// session so `/status` shows exact data (system prompt, tool schemas, allowed
/// tools) instead of the transcript-based reconstruction. Clones share state.
#[derive(Debug, Clone, Default)]
pub struct FinishedStatusRegistry {
    snapshots: Arc<Mutex<HashMap<Uuid, StatusSnapshot>>>,
}

impl FinishedStatusRegistry {
    pub(crate) fn insert(&self, run_id: Uuid, snapshot: StatusSnapshot) {
        self.snapshots
            .lock()
            .expect("finished status registry mutex")
            .insert(run_id, snapshot);
    }

    pub(crate) fn get(&self, run_id: Uuid) -> Option<StatusSnapshot> {
        self.snapshots
            .lock()
            .expect("finished status registry mutex")
            .get(&run_id)
            .cloned()
    }
}

/// Number of calls per tool name across `messages`.
pub(crate) fn tool_call_counts(messages: &[Message]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for message in messages {
        if let Message::Assistant { tool_calls, .. } = message {
            for tool_call in tool_calls {
                *counts.entry(tool_call.name.clone()).or_insert(0) += 1;
            }
        }
    }
    counts
}

impl StatusSnapshot {
    /// Rebuild the status of the finished subagent `run_id` from its
    /// transcript. The system prompt and tool schemas are not persisted, so the
    /// breakdown covers only the conversation (messages and compaction summary).
    /// The allowed tools come from the header and stay empty for transcripts
    /// written before they were recorded.
    pub(crate) fn from_finished_subagent(run_id: Uuid, state: &SessionState) -> Self {
        let mut provider_connection = String::new();
        let mut model = String::new();
        let mut parameters = ModelParameters::default();
        let mut agent_type = None;
        let mut allowed_tools = Vec::new();
        for event in &state.events {
            match event {
                StoreEvent::Meta(meta) => {
                    allowed_tools = meta.allowed_tools.clone().unwrap_or_default();
                    provider_connection = meta.provider_connection.clone();
                    model = meta.model.clone();
                    agent_type = meta.agent_type.clone();
                }
                StoreEvent::ModelChanged {
                    provider_connection: changed_connection,
                    model: changed_model,
                    parameters: changed_parameters,
                    ..
                } => {
                    provider_connection = changed_connection.clone();
                    model = changed_model.clone();
                    parameters = changed_parameters.clone();
                }
                StoreEvent::AgentTypeChanged {
                    agent_type: changed,
                    ..
                } => agent_type = Some(changed.clone()),
                _ => {}
            }
        }
        let context_length = parameters.context_length.unwrap_or(DEFAULT_CONTEXT_LENGTH);

        let mut context = Context::with_system_prompt(String::new());
        context.hydrate(state.messages.clone());
        if let (Some(mut snapshot), Some(boundary)) = (
            state.compaction.clone(),
            state.compaction_transcript_boundary,
        ) {
            snapshot.boundary = boundary + context.leading_system_len();
            context.set_compaction(snapshot);
        }
        let conversation_segments = context
            .context_breakdown(&[], None, context_length)
            .segments()
            .iter()
            .filter(|segment| {
                segment.category.group() != crate::context_breakdown::ContextGroup::SystemPrompt
            })
            .cloned()
            .collect();

        Self {
            run_id: Some(run_id),
            engine_name: parameters.engine_name.unwrap_or_default(),
            provider_connection,
            model,
            reasoning_effort: parameters.reasoning_effort,
            temperature: parameters.temperature,
            max_output_tokens: parameters.max_output_tokens,
            agent_type,
            allowed_tools,
            tool_call_counts: tool_call_counts(&state.messages),
            breakdown: ContextBreakdown::new(conversation_segments, context_length),
            timeline: timeline_from_events(&state.events),
            finished_subagent: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AssistantStatus, Message, UserMessageKind};
    use crate::context_breakdown::ContextGroup;
    use crate::store::{CompactionEffect, CompactionRecord, Store, SubagentSessionHeader};

    #[test]
    fn counts_every_tool_call_by_name() {
        let call = |name: &str| crate::agent::ToolCallRequest {
            id: Uuid::new_v4(),
            name: name.to_string(),
            arguments: serde_json::json!({}),
            persisted_title_arguments: Vec::new(),
        };
        let messages = [Message::Assistant {
            content: String::new(),
            status: AssistantStatus::Complete,
            tool_calls: vec![call("read"), call("read"), call("write")],
        }];
        let counts = tool_call_counts(&messages);
        assert_eq!(counts.get("read"), Some(&2));
        assert_eq!(counts.get("write"), Some(&1));
        assert_eq!(counts.get("search"), None);
    }

    #[tokio::test]
    async fn rebuilds_status_from_a_subagent_transcript() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = Store::create(
            &temp.path().join("chats"),
            temp.path(),
            Uuid::new_v4(),
            "mock",
            "model",
            None,
        )
        .expect("store");
        let run_id = Uuid::new_v4();
        let parameters = ModelParameters {
            engine_name: Some("main".to_string()),
            max_output_tokens: Some(512),
            context_length: Some(1000),
            ..Default::default()
        };
        let effect = CompactionEffect {
            estimated_tokens_before: 400,
            estimated_tokens_after: 100,
        };
        let messages = [
            Message::User {
                content: "task".to_string(),
                kind: UserMessageKind::Explicit,
            },
            Message::Assistant {
                content: "answer".to_string(),
                status: AssistantStatus::Complete,
                tool_calls: Vec::new(),
            },
        ];
        store
            .write_subagent_transcript_for_run(
                run_id,
                &SubagentSessionHeader {
                    provider_connection_and_model: Some(("mock".to_string(), "model".to_string())),
                    agent_type: Some("research".to_string()),
                    parameters,
                    allowed_tools: vec!["read".to_string(), "write".to_string()],
                },
                &messages,
                &[CompactionRecord {
                    boundary: 1,
                    transcript_boundary: Some(1),
                    summary: "summary".to_string(),
                    effect: Some(effect),
                }],
            )
            .await
            .expect("transcript written");

        let state = store.read_subagent_session_state(run_id).expect("state");
        let snapshot = StatusSnapshot::from_finished_subagent(run_id, &state);

        assert!(snapshot.finished_subagent);
        assert_eq!(snapshot.run_id, Some(run_id));
        assert_eq!(snapshot.model, "model");
        assert_eq!(snapshot.engine_name, "main");
        assert_eq!(snapshot.agent_type.as_deref(), Some("research"));
        assert_eq!(snapshot.allowed_tools, ["read", "write"]);
        assert_eq!(snapshot.max_output_tokens, Some(512));
        assert_eq!(snapshot.breakdown.context_length(), 1000);
        assert!(snapshot.timeline.iter().any(|entry| matches!(
            entry,
            crate::agent::StatusTimelineEntry::Compaction { effect: Some(found), .. } if *found == effect
        )));
        assert!(snapshot.breakdown.used_tokens() > 0);
        assert!(snapshot
            .breakdown
            .segments()
            .iter()
            .all(|segment| segment.category.group() != ContextGroup::SystemPrompt));
    }
}
