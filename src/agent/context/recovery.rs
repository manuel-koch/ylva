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
    /// Advance as far as safely possible without summarizing the latest task.
    /// The advancing cut excludes unchanged summaries and recovery handoffs.
    pub(crate) fn manual_summary_boundary(&self) -> Option<usize> {
        let latest_task = self.messages.iter().rposition(|message| {
            matches!(
                message,
                Message::User {
                    kind: UserMessageKind::Explicit,
                    ..
                }
            )
        })?;
        compaction::safe_boundaries_from(&self.messages, self.model_view_start())
            .into_iter()
            .rfind(|boundary| *boundary > self.model_view_start() && *boundary <= latest_task)
    }

    /// Oldest-first token budget the prefix a new summary must cover so the
    /// model view lands at `target_tokens`: the smallest prefix (counted from
    /// the first message in the visible tail) whose removal
    /// reaches the target, snapped forward to a cut that keeps every
    /// tool-call/result group intact.
    ///
    /// `None` when no summarization is useful: the view already fits the
    /// target, or there is no non-system prefix to summarize at all. The
    /// summary's own size is unknown when this runs, so reaching the target
    /// is best-effort — a later sweep trims the remainder.
    pub fn summary_boundary_for_target(&self, target_tokens: usize) -> Option<usize> {
        let view_start = self.model_view_start();
        if self.compaction.is_some()
            && self.entries[view_start..]
                .iter()
                .map(|entry| entry.token_count)
                .sum::<usize>()
                < 128
        {
            return None;
        }
        let tokens_to_remove = self.model_view_token_count().saturating_sub(target_tokens);
        if tokens_to_remove == 0 {
            return None;
        }
        let mut accumulated = 0usize;
        for index in view_start..self.entries.len() {
            accumulated += self.entries[index].token_count;
            if accumulated >= tokens_to_remove {
                return crate::agent::compaction::first_safe_boundary_from(
                    &self.messages,
                    index + 1,
                )
                .or_else(|| {
                    compaction::safe_boundaries_from(&self.messages, view_start)
                        .into_iter()
                        .rfind(|boundary| *boundary <= index + 1)
                })
                .filter(|boundary| *boundary > view_start);
            }
        }
        (self.entries.len() > view_start)
            .then(|| {
                crate::agent::compaction::first_safe_boundary_from(
                    &self.messages,
                    self.entries.len(),
                )
            })
            .flatten()
            .filter(|boundary| *boundary > view_start)
    }

    /// Find the oldest safe prefix whose removal brings model-visible content
    /// at or below `target_tokens`. The generated summary is not available yet,
    /// so it is excluded from this projection.
    pub(crate) fn summary_boundary_for_model_target(
        &self,
        target_tokens: usize,
        tools: &[ToolSchema],
        _engine: &dyn Engine,
    ) -> Option<usize> {
        let _ = _engine;
        let minimum_boundary = self.summary_boundary_for_target(
            target_tokens.saturating_sub(self.tool_schema_token_count(tools)),
        )?;
        compaction::safe_boundaries_from(&self.messages, self.model_view_start())
            .into_iter()
            .find(|boundary| {
                if *boundary < minimum_boundary {
                    return false;
                }
                let latest_request = self.messages.iter().rposition(|message| {
                    matches!(
                        message,
                        Message::User {
                            kind: UserMessageKind::Explicit,
                            ..
                        }
                    )
                });
                (self.model_view_start()..*boundary)
                    .filter(|index| Some(*index) != latest_request)
                    .map(|index| self.entries[index].token_count)
                    .sum::<usize>()
                    >= 128
            })
    }

    /// Synchronously drop model-view messages — oldest first — until the view
    /// fits `target_tokens`.
    ///
    /// Only complete turns are cut: every candidate position keeps each
    /// `tool_call_id` together with its one result, the leading system
    /// instructions and any applied summary stay, and the transcript itself
    /// is untouched. Returns `true` when the target is met; `false` when even
    /// the empty tail would not fit (nothing is dropped then).
    pub fn drop_model_view_to(&mut self, target_tokens: usize) -> bool {
        let from = self.model_view_start();
        let head_tokens = self.model_view_head_tokens();
        for cut in crate::agent::compaction::safe_boundaries_from(&self.messages, from) {
            let tail_tokens: usize = self.entries[cut..]
                .iter()
                .map(|entry| entry.token_count)
                .sum();
            if head_tokens + tail_tokens <= target_tokens {
                self.view_start = self.view_start.max(cut);
                return true;
            }
        }
        false
    }

    /// Lossy, continuity-preserving recovery of the model view.
    ///
    /// Drops the oldest complete tool groups until the view (tool schemas
    /// included) fits `target_tokens`; when the target is unattainable it
    /// retains as much recent content as fits `ceiling_tokens`, and when even
    /// that is impossible it keeps only the projection. Every drop installs a
    /// [`RecoveryProjection`](crate::agent::compaction::recovery::RecoveryProjection):
    /// a handoff of what was removed plus the newest explicit user request,
    /// so a continuation never starts from system messages alone.
    pub(crate) fn recover_model_view(
        &mut self,
        target_tokens: usize,
        ceiling_tokens: usize,
        tools: &[ToolSchema],
    ) -> crate::agent::compaction::recovery::RecoveryReport {
        self.recover_projection(target_tokens, ceiling_tokens, tools)
    }

    fn recover_projection(
        &mut self,
        target_tokens: usize,
        ceiling_tokens: usize,
        tools: &[ToolSchema],
    ) -> compaction::recovery::RecoveryReport {
        use compaction::recovery::{
            build_projection, marked_text, ProjectionBudget, RecoveryReport,
            REQUEST_OMISSION_MARKER, SUMMARY_OMISSION_MARKER,
        };
        let schema_tokens = self.tool_schema_token_count(tools);
        let target = target_tokens.saturating_sub(schema_tokens);
        let ceiling = ceiling_tokens.saturating_sub(schema_tokens);
        let tokens_before = self.model_view_token_count();
        let from = self.model_view_start();
        let latest_request = self.messages.iter().rposition(|message| {
            matches!(
                message,
                Message::User {
                    kind: UserMessageKind::Explicit,
                    ..
                }
            )
        });
        let newest_batch = (from..self.messages.len()).rev().find(|index| matches!(
            &self.messages[*index], Message::Assistant { tool_calls, .. } if !tool_calls.is_empty()
        ));
        let retained_start =
            newest_batch.unwrap_or_else(|| latest_request.unwrap_or(from).max(from));
        let mut changed = false;
        let mut stage = 0;
        let mut clipped_results = 0;
        let body_allowance = (target / 16).max(MINIMUM_USEFUL_RESULT_TOKENS);
        if self.model_view_token_count() > target {
            stage = 1;
            for index in from..retained_start {
                if self.limit_model_message(index, body_allowance) {
                    changed = true;
                    clipped_results += 1;
                }
            }
        }
        if self.model_view_token_count() > target {
            stage = 2;
            let immutable_batch_tokens = newest_batch.map_or(0, |index| {
                self.messages[index..]
                    .iter()
                    .map(|message| {
                        let projected =
                            compaction::recovery::project_body(self.estimator, message, 64);
                        estimate_message_tokens(
                            self.estimator,
                            projected.as_ref().unwrap_or(message),
                        )
                    })
                    .sum::<usize>()
            });
            let keep_newest =
                immutable_batch_tokens + self.model_view_base_tokens() + 192 < ceiling;
            let cuts = compaction::safe_boundaries_from(&self.messages, from);
            for cut in cuts
                .into_iter()
                .filter(|cut| *cut > from && (!keep_newest || *cut <= retained_start))
            {
                let projection = build_projection(
                    self.estimator,
                    &self.messages,
                    from,
                    cut,
                    self.recovery.as_ref(),
                    ProjectionBudget {
                        handoff_tokens: (target / 8).max(128),
                        anchor_tokens: usize::MAX,
                    },
                );
                self.view_start = cut;
                self.recovery = Some(projection);
                self.model_overrides.retain(|index, _| *index >= cut);
                changed = true;
                if self.model_view_token_count() <= target {
                    break;
                }
            }
        }
        if self.model_view_token_count() > target {
            stage = 3;
            let result_indices: Vec<usize> = (self.model_view_start()..self.messages.len())
                .filter(|index| {
                    matches!(
                        &self.messages[*index],
                        Message::ToolResult { .. }
                            | Message::ToolError { .. }
                            | Message::User {
                                kind: UserMessageKind::ImplicitToolNote,
                                ..
                            }
                    )
                })
                .collect();
            let allowance = (target / (result_indices.len() + 4)).max(64);
            for index in result_indices {
                if self.limit_model_message(index, allowance) {
                    changed = true;
                    clipped_results += 1;
                }
            }
        }
        if self.model_view_token_count() > target {
            stage = 4;
            if let Some(snapshot) = &self.compaction {
                if let Message::User { content, .. } = self.projected_summary(snapshot) {
                    let shortened = compaction::recovery::marked_prompt_block(
                        self.estimator,
                        PromptBlockKind::CompactionSummary,
                        &content,
                        SUMMARY_OMISSION_MARKER,
                        128,
                    );
                    if shortened != content {
                        self.summary_override = Some(shortened);
                        changed = true;
                    }
                }
            }
            if let Some(recovery) = &mut self.recovery {
                if let Message::User { content, .. } = &mut recovery.handoff {
                    let shortened = compaction::recovery::marked_prompt_block(
                        self.estimator,
                        PromptBlockKind::RecoveryHandoff,
                        content,
                        SUMMARY_OMISSION_MARKER,
                        128,
                    );
                    changed |= shortened != *content;
                    *content = shortened;
                }
                recovery.token_count = estimate_message_tokens(self.estimator, &recovery.handoff)
                    + recovery.anchor.as_ref().map_or(0, |message| {
                        estimate_message_tokens(self.estimator, message)
                    });
            }
        }
        // Missing the target alone is not a reason to shorten the task.
        if self.model_view_token_count() > ceiling {
            stage = 5;
            let anchor_in_recovery = latest_request
                .is_some_and(|index| index < self.model_view_start())
                && self
                    .recovery
                    .as_ref()
                    .is_some_and(|recovery| recovery.anchor.is_some());
            let anchor = if anchor_in_recovery {
                self.recovery
                    .as_ref()
                    .and_then(|recovery| recovery.anchor.clone())
            } else {
                latest_request.map(|index| {
                    self.model_overrides
                        .get(&index)
                        .unwrap_or(&self.messages[index])
                        .clone()
                })
            };
            if let Some(Message::User { content, kind }) = anchor {
                let other_tokens = self.model_view_token_count().saturating_sub(
                    self.estimator.estimate_tokens(&content) + TOKEN_FRAMING_OVERHEAD,
                );
                let allowance = ceiling.saturating_sub(other_tokens + TOKEN_FRAMING_OVERHEAD);
                let shortened =
                    marked_text(self.estimator, &content, REQUEST_OMISSION_MARKER, allowance);
                changed |= shortened != content;
                let message = Message::User {
                    content: shortened,
                    kind,
                };
                if let Some(recovery) = &mut self.recovery {
                    if anchor_in_recovery {
                        recovery.anchor = Some(message.clone());
                        recovery.token_count =
                            estimate_message_tokens(self.estimator, &recovery.handoff)
                                + estimate_message_tokens(self.estimator, &message);
                    } else if let Some(index) = latest_request {
                        self.model_overrides.insert(index, message);
                    }
                } else if let Some(index) = latest_request {
                    self.model_overrides.insert(index, message);
                }
            }
        }
        let estimated_tokens_after = self.model_view_token_count();
        self.projection_dirty |= changed;
        let model_messages = self.model_messages();
        let has_task = model_messages.iter().rev().find(|message| matches!(
            message, Message::User { kind: UserMessageKind::Explicit, .. }
        )).is_some_and(|message| matches!(
            message, Message::User { content, kind: UserMessageKind::Explicit }
                if !content.trim().is_empty()
                    && content.strip_suffix(REQUEST_OMISSION_MARKER).is_none_or(|prefix| !prefix.trim().is_empty())
        ));
        tracing::info!(
            trigger = self.compaction_trigger,
            request_id = ?self.compaction_request_id,
            safe_boundary = self.model_view_start(),
            stage,
            clipped_results,
            estimated_tokens_before = tokens_before + schema_tokens,
            estimated_tokens_after = estimated_tokens_after + schema_tokens,
            target_tokens,
            ceiling_tokens,
            projected_messages = self.model_view_message_count(),
            admitted_calls = self.batch_admitted_calls,
            skipped_calls = self.batch_skipped_calls,
            batch_cap = MAX_EXECUTED_TOOL_CALLS_PER_BATCH,
            remaining_result_allowance = ceiling.saturating_sub(estimated_tokens_after),
            terminal_reason = if estimated_tokens_after <= ceiling && has_task {
                "fits"
            } else {
                "irreducible"
            },
            "compaction recovery projection evaluated"
        );
        RecoveryReport {
            changed,
            estimated_tokens_after,
            fits_target: estimated_tokens_after <= target,
            fits_ceiling: estimated_tokens_after <= ceiling && has_task,
            stage,
            clipped_results,
        }
    }
}
