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

use super::*;

impl Context {
    /// Estimate an exact manual candidate without changing any projection state.
    /// Its cut must still advance and preserve the current latest explicit task.
    pub(in crate::agent) fn manual_compaction_estimate(
        &self,
        snapshot: &compaction::CompactionSnapshot,
        tools: &[ToolSchema],
    ) -> Option<usize> {
        let boundary = snapshot.boundary;
        let latest_task = self.messages.iter().rposition(|message| {
            matches!(
                message,
                Message::User {
                    kind: UserMessageKind::Explicit,
                    ..
                }
            )
        })?;
        if snapshot.summary.trim().is_empty()
            || boundary <= self.model_view_start()
            || boundary > latest_task
            || !compaction::safe_boundaries_from(&self.messages, self.leading_system_len())
                .contains(&boundary)
        {
            return None;
        }
        let system_tokens: usize = self.entries[..self.leading_system_len()]
            .iter()
            .map(|entry| entry.token_count)
            .sum();
        let tail_tokens: usize = (boundary..self.messages.len())
            .map(|index| {
                self.model_overrides
                    .get(&index)
                    .map_or(self.entries[index].token_count, |message| {
                        estimate_message_tokens(self.estimator, message)
                    })
            })
            .sum();
        Some(
            system_tokens
                + estimate_message_tokens(self.estimator, &snapshot.summary_message())
                + tail_tokens
                + self.tool_schema_token_count(tools),
        )
    }

    pub(in crate::agent) fn project_last_harness_notice(&mut self, notice: String) {
        let index = self.messages.len() - 1;
        assert!(matches!(
            self.messages[index],
            Message::User {
                kind: UserMessageKind::ImplicitToolNote,
                ..
            }
        ));
        self.model_overrides.insert(
            index,
            Message::User {
                content: format!("{HARNESS_NOTICE_MARKER}\n{notice}"),
                kind: UserMessageKind::ImplicitToolNote,
            },
        );
        self.projection_dirty = true;
    }

    pub(in crate::agent) fn summary_source(&self, boundary: usize) -> Vec<Message> {
        let mut prefix = Vec::new();
        if let Some(snapshot) = self.compaction() {
            prefix.push(snapshot.summary_message());
        }
        if let Some(recovery) = &self.recovery {
            prefix.push(recovery.handoff.clone());
            prefix.extend(recovery.anchor.clone());
        }
        prefix.extend_from_slice(&self.messages[self.model_view_start()..boundary]);
        prefix
    }

    /// First transcript index included in the model view after the leading
    /// system block.
    pub(super) fn model_view_start(&self) -> usize {
        self.view_start.max(self.leading_system_len())
    }

    /// Tokens of the model view that are *not* part of the verbatim tail:
    /// the leading system block plus, when compacted, the projected summary.
    pub(super) fn model_view_head_tokens(&self) -> usize {
        self.model_view_base_tokens()
            + self
                .recovery
                .as_ref()
                .map_or(0, |recovery| recovery.token_count)
            + self.projected_task_anchor().map_or(0, |message| {
                estimate_message_tokens(self.estimator, message)
            })
    }

    pub(super) fn projected_task_anchor(&self) -> Option<&Message> {
        if self
            .recovery
            .as_ref()
            .is_some_and(|recovery| recovery.anchor.is_some())
        {
            return None;
        }
        self.messages
            .iter()
            .enumerate()
            .rev()
            .find(|(_, message)| {
                matches!(
                    message,
                    Message::User {
                        kind: UserMessageKind::Explicit,
                        ..
                    }
                )
            })
            .filter(|(index, _)| *index < self.model_view_start())
            .map(|(index, message)| self.model_overrides.get(&index).unwrap_or(message))
    }

    /// Leading system block plus the projected summary, without any
    /// recovery projection.
    pub(super) fn model_view_base_tokens(&self) -> usize {
        let leading_system_len = self.leading_system_len();
        let mut tokens: usize = self.entries[..leading_system_len]
            .iter()
            .map(|entry| entry.token_count)
            .sum();
        if let Some(snapshot) = &self.compaction {
            tokens += estimate_message_tokens(self.estimator, &self.projected_summary(snapshot));
        }
        tokens
    }

    /// The model-facing view of the conversation for the next request.
    ///
    /// The transcript stays intact no matter what the model sees: the
    /// leading system instructions, then the marked summary of the compacted
    /// prefix (when one is applied), then the verbatim tail after the
    /// boundary. Without a compaction snapshot — and before
    /// the hard fallback drops anything — this is simply the whole transcript
    /// in order.
    pub fn model_messages(&self) -> Vec<Message> {
        self.model_messages_from(self.model_view_start())
    }

    fn model_messages_from(&self, view_start: usize) -> Vec<Message> {
        let leading_system_len = self.leading_system_len();
        let mut messages: Vec<Message> = self.messages[..leading_system_len].to_vec();
        if let Some(snapshot) = &self.compaction {
            messages.push(self.projected_summary(snapshot));
        }
        if let Some(recovery) = &self.recovery {
            messages.push(recovery.handoff.clone());
            messages.extend(recovery.anchor.clone());
        }
        messages.extend(self.projected_task_anchor().cloned());
        messages.extend(
            (view_start.max(leading_system_len)..self.messages.len()).map(|index| {
                self.model_overrides
                    .get(&index)
                    .unwrap_or(&self.messages[index])
                    .clone()
            }),
        );
        messages
    }

    pub(super) fn projected_summary(&self, snapshot: &compaction::CompactionSnapshot) -> Message {
        self.summary_override.as_ref().map_or_else(
            || snapshot.summary_message(),
            |content| Message::User {
                content: content.clone(),
                kind: UserMessageKind::ImplicitToolNote,
            },
        )
    }

    pub(in crate::agent) fn limit_last_model_message(&mut self, allowance: usize) {
        if let Some(index) = self.messages.len().checked_sub(1) {
            self.limit_model_message(index, allowance);
        }
    }

    pub(super) fn limit_model_message(&mut self, index: usize, allowance: usize) -> bool {
        let message = self
            .model_overrides
            .get(&index)
            .unwrap_or(&self.messages[index]);
        if let Some(projected) =
            compaction::recovery::project_body(self.estimator, message, allowance)
        {
            self.model_overrides.insert(index, projected);
            self.projection_dirty = true;
            return true;
        }
        false
    }

    pub(in crate::agent) fn persist_model_projection(&mut self) {
        if !self.projection_dirty {
            return;
        }
        let Some(store) = self.store() else {
            return;
        };
        self.projection_dirty = false;
        self.projection_revision += 1;
        let leading = self.leading_system_len();
        let mut message_overrides: Vec<(usize, Message)> = self
            .model_overrides
            .iter()
            .map(|(index, message)| (index.saturating_sub(leading), message.clone()))
            .collect();
        message_overrides.sort_by_key(|(index, _)| *index);
        store.record_model_projection(compaction::ModelProjectionSnapshot {
            projection_revision: self.projection_revision,
            transcript_start: self.model_view_start().saturating_sub(leading),
            message_overrides,
            summary_override: self.summary_override.clone(),
            handoff: self
                .recovery
                .as_ref()
                .map(|recovery| recovery.handoff.clone()),
            anchor: self
                .recovery
                .as_ref()
                .and_then(|recovery| recovery.anchor.clone()),
        });
    }

    pub(super) fn restore_model_projection(
        &mut self,
        projection: compaction::ModelProjectionSnapshot,
    ) {
        self.projection_revision = projection.projection_revision;
        let valid_handoff = projection.handoff.as_ref().is_none_or(|message| {
            matches!(
                message,
                Message::User {
                    kind: UserMessageKind::ImplicitToolNote,
                    ..
                }
            )
        });
        let valid_anchor = projection.anchor.as_ref().is_none_or(|message| {
            matches!(
                message,
                Message::User {
                    kind: UserMessageKind::Explicit,
                    ..
                }
            )
        });
        if !valid_handoff
            || !valid_anchor
            || (projection.anchor.is_some() && projection.handoff.is_none())
        {
            tracing::warn!("ignoring invalid persisted recovery message types");
            return;
        }
        let leading = self.leading_system_len();
        let boundary = projection.transcript_start.saturating_add(leading);
        if boundary < self.model_view_start()
            || boundary > self.messages.len()
            || !compaction::safe_boundaries_from(&self.messages, leading).contains(&boundary)
        {
            tracing::warn!(
                boundary,
                "ignoring invalid persisted model projection boundary"
            );
            return;
        }
        self.view_start = self.view_start.max(boundary);
        self.summary_override = projection.summary_override;
        for (index, message) in projection.message_overrides {
            let index = index.saturating_add(leading);
            let valid =
                self.messages
                    .get(index)
                    .is_some_and(|original| match (original, &message) {
                        (
                            Message::ToolResult { value: original },
                            Message::ToolResult { value: projected },
                        ) => original.id == projected.id && original.name == projected.name,
                        (
                            Message::ToolError {
                                id: original,
                                name: original_name,
                                ..
                            },
                            Message::ToolError {
                                id: projected,
                                name: projected_name,
                                ..
                            },
                        ) => original == projected && original_name == projected_name,
                        (
                            Message::User { kind: original, .. },
                            Message::User {
                                kind: projected, ..
                            },
                        ) => original == projected,
                        _ => false,
                    });
            if valid {
                self.model_overrides.insert(index, message);
            } else {
                tracing::warn!(index, "ignoring invalid persisted model message override");
            }
        }
        if let Some(handoff) = projection.handoff {
            let token_count = estimate_message_tokens(self.estimator, &handoff)
                + projection
                    .anchor
                    .as_ref()
                    .map_or(0, |anchor| estimate_message_tokens(self.estimator, anchor));
            self.recovery = Some(compaction::recovery::RecoveryProjection {
                boundary,
                handoff,
                anchor: projection.anchor,
                token_count,
            });
        }
        self.projection_dirty = false;
    }
}
