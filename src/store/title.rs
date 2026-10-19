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

//! Session title inputs, prompt construction and title rewrite.

use tokio::sync::oneshot;

use super::writer::WriterCommand;
use super::{SessionMeta, Store, StoreError};
use crate::agent::tools::agent_finish::{AgentFinishArgs, AgentFinishStatus, AgentFinishTool};
use crate::agent::{Message, UserMessageKind};

/// Rebuild the title inputs from committed transcript messages.
pub(super) fn title_snapshot(meta: &SessionMeta, messages: &[Message]) -> TitleSnapshot {
    let mut snapshot = TitleSnapshot {
        title: meta.title.clone(),
        completed_subagent_outcome: if meta.parent_session.is_some() {
            subagent_title_outcome(messages)
        } else {
            None
        },
        ..TitleSnapshot::default()
    };
    for message in messages {
        match message {
            Message::User {
                content,
                kind: UserMessageKind::Explicit,
            } if snapshot.first_user_message.is_none() => {
                snapshot.first_user_message = Some(content.clone());
            }
            Message::Assistant {
                content,
                status: crate::agent::AssistantStatus::Complete,
                ..
            } if snapshot.first_user_message.is_some()
                && snapshot.first_complete_assistant_message.is_none()
                && !content.trim().is_empty() =>
            {
                snapshot.first_complete_assistant_message = Some(content.clone());
            }
            _ => {}
        }
    }
    snapshot
}

impl Store {
    /// Return the currently persisted session title, or an empty string while
    /// title generation is still pending.
    pub fn title(&self) -> String {
        self.lock_title_snapshot().title.clone()
    }

    /// A poisoned lock only means another thread panicked mid-update of plain
    /// title data, which stays usable.
    pub(super) fn lock_title_snapshot(&self) -> std::sync::MutexGuard<'_, TitleSnapshot> {
        self.inner
            .title_snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Return the utility-engine prompt from the first explicit user message
    /// and a completed reply or a subagent's structured terminal outcome.
    pub fn title_prompt(&self) -> Option<String> {
        let snapshot = self.lock_title_snapshot();
        if !snapshot.title.is_empty() {
            tracing::debug!(
                session_id = %self.inner.session_id,
                stage = "skipped",
                reason = "title_already_set",
                "session title generation skipped"
            );
            return None;
        }
        let Some(user_message) = snapshot.first_user_message.as_deref() else {
            tracing::debug!(
                session_id = %self.inner.session_id,
                stage = "skipped",
                reason = "awaiting_explicit_user_message",
                "session title generation is not ready"
            );
            return None;
        };
        let (response_kind, response_message) =
            if let Some(outcome) = &snapshot.completed_subagent_outcome {
                let response_kind = match outcome.status {
                    AgentFinishStatus::Success => "subagent_result",
                    AgentFinishStatus::Failure => "subagent_error",
                };
                (response_kind, outcome.content.as_str())
            } else {
                let Some(assistant_message) = snapshot.first_complete_assistant_message.as_deref()
                else {
                    tracing::debug!(
                        session_id = %self.inner.session_id,
                        stage = "skipped",
                        reason = "awaiting_complete_assistant_reply",
                        "session title generation is not ready"
                    );
                    return None;
                };
                ("assistant_message", assistant_message)
            };
        let prompt = format!(
            "Generate a concise title for this conversation.\n\
             Return only one bare title, with no markdown, quotation marks, or explanation.\
             \n\n<user_message>\n{user_message}\n</user_message>\n\
             <{response_kind}>\n{response_message}\n</{response_kind}>",
        );
        tracing::debug!(
            session_id = %self.inner.session_id,
            stage = "prompt_ready",
            user_message_chars = user_message.chars().count(),
            response_kind,
            response_message_chars = response_message.chars().count(),
            prompt_chars = prompt.chars().count(),
            "session title prompt is ready"
        );
        Some(prompt)
    }

    /// Enqueue a metadata-only title rewrite on the session writer.
    pub async fn set_title(&self, title: String) -> Result<(), StoreError> {
        if !self.lock_title_snapshot().title.is_empty() {
            return Ok(());
        }
        let (result_tx, result_rx) = oneshot::channel();
        let title_for_snapshot = title.clone();
        self.inner
            .command_tx
            .send(WriterCommand::SetTitle { title, result_tx })
            .await
            .map_err(|_| StoreError::WriterClosed {
                session_id: self.inner.session_id,
            })?;
        let result = result_rx.await.map_err(|_| StoreError::WriterClosed {
            session_id: self.inner.session_id,
        })?;
        result?;
        let mut snapshot = self.lock_title_snapshot();
        snapshot.title = title_for_snapshot;
        Ok(())
    }

    pub(super) fn update_title_snapshot(&self, message: &Message) {
        let mut snapshot = self.lock_title_snapshot();
        match message {
            Message::User {
                content,
                kind: UserMessageKind::Explicit,
            } if snapshot.first_user_message.is_none() => {
                snapshot.first_user_message = Some(content.clone());
            }
            Message::Assistant {
                content,
                status: crate::agent::AssistantStatus::Complete,
                ..
            } if snapshot.first_user_message.is_some()
                && snapshot.first_complete_assistant_message.is_none()
                && !content.trim().is_empty() =>
            {
                snapshot.first_complete_assistant_message = Some(content.clone());
            }
            _ => {}
        }
    }
}

/// The title rewrite state carried into the spawned writer task.
pub(super) struct TitleState {
    /// `Some((meta, meta_line_len))` until a generated title is persisted:
    /// `meta` is the header as currently written to disk (title still
    /// empty), and `meta_line_len` is the byte length of that first line
    /// (JSON plus newline), i.e. where the second line begins — needed to
    /// rewrite just the first line once the title is known. `None` if this
    /// session's header already carried a title.
    pub(super) meta_awaiting_title: Option<(SessionMeta, u64)>,
}

pub(super) struct SubagentTitleOutcome {
    pub(super) status: AgentFinishStatus,
    pub(super) content: String,
}

pub(super) fn subagent_title_outcome(messages: &[Message]) -> Option<SubagentTitleOutcome> {
    let completed_calls: std::collections::BTreeSet<_> = messages
        .iter()
        .filter_map(|message| match message {
            Message::ToolResult { value } if value.name == AgentFinishTool::NAME => Some(value.id),
            _ => None,
        })
        .collect();
    for message in messages.iter().rev() {
        let Message::Assistant { tool_calls, .. } = message else {
            continue;
        };
        for call in tool_calls.iter().rev() {
            if call.name != AgentFinishTool::NAME || !completed_calls.contains(&call.id) {
                continue;
            }
            let arguments: AgentFinishArgs = match serde_json::from_value(call.arguments.clone()) {
                Ok(arguments) => arguments,
                Err(_) => {
                    // Deserialization errors can quote private outcome content.
                    tracing::warn!(tool_call_id = %call.id, reason = "invalid_agent_finish_arguments",
                        "completed subagent outcome could not be decoded for session title");
                    continue;
                }
            };
            let status = arguments.resolved_status();
            let content = match status {
                AgentFinishStatus::Success => arguments.result,
                AgentFinishStatus::Failure => arguments.error,
            };
            if let Some(content) = content.filter(|content| !content.trim().is_empty()) {
                return Some(SubagentTitleOutcome { status, content });
            }
            tracing::debug!(tool_call_id = %call.id, reason = "empty_subagent_outcome",
                "completed subagent outcome contains no title input");
        }
    }
    None
}

#[derive(Default)]
pub(super) struct TitleSnapshot {
    pub(super) title: String,
    pub(super) first_user_message: Option<String>,
    pub(super) first_complete_assistant_message: Option<String>,
    pub(super) completed_subagent_outcome: Option<SubagentTitleOutcome>,
}
