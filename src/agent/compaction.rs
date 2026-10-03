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

//! Automatic context compaction for root conversations (blueprint §4C;
//! `docs/ideas/conversation-compaction.md`).
//!
//! The transcript in [`Context`](crate::agent::Context) is never rewritten.
//! Compaction only changes the *model view*: the leading system instructions
//! stay verbatim, the summarized prefix becomes one marked summary entry, and
//! the tail after the boundary is sent as it happened.
//!
//! Scheduling follows the selected option C: a completed turn above
//! [`DEFAULT_SOFT_COMPACTION_THRESHOLD`] starts a background summary through the
//! configured `compact_engine`, which the outer loop applies at the next safe
//! boundary. Above [`DEFAULT_HARD_COMPACTION_THRESHOLD`] — or when the summary fails
//! or times out — the loop drops old messages synchronously instead.

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agent::{Context, Message, StreamEvent, UserMessageKind};
use crate::llm::engine::Engine;
use tokio::sync::mpsc;

/// Default usage fraction of the active context window at or above which a
/// completed turn starts a background summary (blueprint §4C).
///
/// The blueprint keeps both triggers fixed — there is no profile knob for
/// them — so today the `DEFAULT_` name only records that this is the value a
/// future override would start from.
pub const DEFAULT_SOFT_COMPACTION_THRESHOLD: f64 = 0.70;

/// Default usage fraction above which the loop stops waiting for a
/// background summary and drops synchronously (blueprint §4C). See
/// [`DEFAULT_SOFT_COMPACTION_THRESHOLD`] for why it is a `DEFAULT_`.
pub const DEFAULT_HARD_COMPACTION_THRESHOLD: f64 = 0.90;

/// Marker prefixing the summarized prefix in the model view.
///
/// The summary is projected as a `user` message — never a mid-conversation
/// `system` message, which several open-weight chat templates reject — and
/// this marker lets the model tell it apart from something the human typed,
/// the same way [`crate::agent::HARNESS_NOTICE_MARKER`] does for harness
/// notices.
pub const COMPACTION_SUMMARY_MARKER: &str = "[compacted conversation summary — not from the user]";

/// Wall-clock budget for one background summary request. Exceeding it is
/// treated like a failure: the loop logs it and falls back to dropping.
pub const COMPACTION_SUMMARY_TIMEOUT: Duration = Duration::from_secs(60);

/// System instruction framing the prefix snapshot handed to the compact
/// engine. The text ships as [`DEFAULT_COMPACTION_PROMPT`] and a workspace or
/// Ylva-home `COMPACTION.md` overrides it (see
/// [`crate::config::compaction_prompt`]).
pub use crate::config::compaction_prompt::DEFAULT_COMPACTION_PROMPT as COMPACTION_SUMMARY_PROMPT;

/// The latest applied (or to-be-applied) compaction: transcript messages
/// `[..boundary]` are represented in the model view by [`Self::summary`].
///
/// The transcript itself keeps every one of those messages — the snapshot
/// only describes the model-facing view (blueprint §4C *record vs view*).
/// Boundaries advance monotonically: a newer snapshot supersedes an older
/// one, and persistence appends one event per snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionSnapshot {
    /// First transcript index kept verbatim as the tail; everything before it
    /// (after the leading system instructions) is summarized.
    pub boundary: usize,
    /// The compact engine's summary of the prefix through `boundary`.
    pub summary: String,
}

impl CompactionSnapshot {
    /// The provider-safe projection of the summarized prefix: a `user`
    /// message carrying [`COMPACTION_SUMMARY_MARKER`], never a
    /// mid-conversation `system` message.
    pub fn summary_message(&self) -> Message {
        Message::User {
            content: format!("{COMPACTION_SUMMARY_MARKER}\n\n{}", self.summary),
            kind: UserMessageKind::ImplicitToolNote,
        }
    }
}

/// What one usage evaluation calls the outer loop to do (blueprint §4C).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionAction {
    /// Below the soft threshold, or a summary is already in flight: do
    /// nothing.
    None,
    /// At or above the soft threshold with nothing in flight: start a
    /// background summary of the old prefix.
    StartSummary,
    /// Above the hard threshold (or a forced retry after a failed summary):
    /// drop old messages synchronously, without waiting.
    DropFallback,
}

/// Map one usage evaluation onto the action it calls for, given the active
/// engine's soft and hard triggers.
///
/// The hard threshold is checked first and wins even while a summary is in
/// flight: once usage passes it the loop may not wait for a background task.
pub fn plan_compaction(
    usage_ratio: f64,
    summary_in_flight: bool,
    soft_threshold: f64,
    hard_threshold: f64,
) -> CompactionAction {
    if usage_ratio > hard_threshold {
        CompactionAction::DropFallback
    } else if usage_ratio >= soft_threshold && !summary_in_flight {
        CompactionAction::StartSummary
    } else {
        CompactionAction::None
    }
}

/// The numbers one sweep measures against: the active engine's effective
/// context window plus its three compaction knobs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompactionBudget {
    /// Effective window: profile value, else the catalog length adopted by
    /// the last `/models` switch, else the built-in default.
    pub context_length: usize,
    /// Fraction of the window a compaction shrinks the model view to.
    pub target_ratio: f64,
    /// Usage at/above which a completed turn starts a background summary.
    pub soft_threshold: f64,
    /// Usage above which the loop drops synchronously.
    pub hard_threshold: f64,
}

/// Estimated change to the model view caused by one compaction operation.
/// Positive savings mean the estimated view became smaller; negative savings
/// mean a generated summary increased the estimated size.
#[derive(Debug, Clone, Copy, PartialEq)]
struct CompactionEffectiveness {
    estimated_tokens_before: usize,
    estimated_tokens_after: usize,
    estimated_token_savings: i64,
    estimated_reduction_percent: f64,
    model_view_messages_before: usize,
    model_view_messages_after: usize,
}

impl CompactionEffectiveness {
    fn measure(
        estimated_tokens_before: usize,
        estimated_tokens_after: usize,
        model_view_messages_before: usize,
        model_view_messages_after: usize,
    ) -> Self {
        let estimated_token_savings =
            saturating_i64(estimated_tokens_before) - saturating_i64(estimated_tokens_after);
        let estimated_reduction_percent = if estimated_tokens_before == 0 {
            0.0
        } else {
            estimated_token_savings as f64 / estimated_tokens_before as f64 * 100.0
        };
        Self {
            estimated_tokens_before,
            estimated_tokens_after,
            estimated_token_savings,
            estimated_reduction_percent,
            model_view_messages_before,
            model_view_messages_after,
        }
    }

    fn removed_model_view_messages(self) -> usize {
        self.model_view_messages_before
            .saturating_sub(self.model_view_messages_after)
    }
}

fn saturating_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

impl CompactionBudget {
    /// Read the budget off the engine currently serving the loop: its
    /// effective window plus the three compaction knobs from the profile.
    pub fn from_engine(engine: &dyn Engine) -> Option<Self> {
        Self::new(engine.effective_context_length(), &engine.config())
    }

    /// Combine a resolved window with an engine config. Returns `None` for a
    /// degenerate zero-length window, so a sweep can never divide by zero.
    fn new(context_length: usize, config: &crate::llm::EngineConfig) -> Option<Self> {
        if context_length == 0 {
            return None;
        }
        Some(Self {
            context_length,
            target_ratio: config.compact_target_ratio,
            soft_threshold: config.soft_compaction_threshold,
            hard_threshold: config.hard_compaction_threshold,
        })
    }

    /// Token budget a synchronous drop must reach.
    pub fn target_tokens(self) -> usize {
        (self.context_length as f64 * self.target_ratio) as usize
    }

    /// Current usage of the model view against this window.
    pub fn usage_ratio(self, context: &Context) -> f64 {
        context.estimated_usage_ratio(self.context_length)
    }
}

/// Every cut position `≥ from` (and `≤ messages.len()`) at which no
/// assistant tool call in `messages[..cut]` is still missing its result —
/// i.e. every cut that keeps each `tool_call_id` together with its one
/// matching [`Message::ToolResult`] or [`Message::ToolError`].
///
/// Compaction may only cut at one of these positions (blueprint §4C *tool
/// protocol constraints*).
pub fn safe_boundaries_from(messages: &[Message], from: usize) -> Vec<usize> {
    let mut open_calls: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
    let mut boundaries = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        if index >= from && open_calls.is_empty() {
            boundaries.push(index);
        }
        match message {
            Message::Assistant { tool_calls, .. } => {
                open_calls.extend(tool_calls.iter().map(|tool_call| tool_call.id));
            }
            Message::ToolResult { value } => {
                open_calls.remove(&value.id);
            }
            Message::ToolError { id, .. } => {
                open_calls.remove(id);
            }
            _ => {}
        }
    }
    if messages.len() >= from && open_calls.is_empty() {
        boundaries.push(messages.len());
    }
    boundaries
}

/// The first cut position `≥ from` that never splits a tool-call/result
/// group, or `None` when the tail carries an unresolved call all the way to
/// the end (compaction must then not cut at all).
pub fn first_safe_boundary_from(messages: &[Message], from: usize) -> Option<usize> {
    safe_boundaries_from(messages, from).first().copied()
}

/// Build the tool-free request sent to the compact engine: the framing system
/// instruction (`prompt`, the loaded `COMPACTION.md` or its built-in
/// default), the immutable prefix snapshot, and a trailing instruction so the
/// model answers even when the prefix ends in a tool result.
pub fn build_summary_request(prefix: &[Message], prompt: &str) -> Vec<Message> {
    let mut request = Vec::with_capacity(prefix.len() + 2);
    request.push(Message::System {
        content: prompt.to_string(),
    });
    request.extend(prefix.iter().cloned());
    request.push(Message::User {
        content: "Write the summary now.".to_string(),
        kind: UserMessageKind::ImplicitToolNote,
    });
    request
}

/// Result of one background summary attempt, delivered back to the outer
/// loop (which — and only which — mutates [`Context`](crate::agent::Context)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactionOutcome {
    /// The compact engine returned a summary for the prefix up to `boundary`.
    SummaryReady {
        /// The loop generation the request was stamped with, so a result
        /// started before `AgentCommand::ClearContext` is dropped instead of
        /// being applied to an unrelated conversation.
        generation: u64,
        boundary: usize,
        summary: String,
    },
    /// The summary failed, timed out, or came back empty: the loop falls
    /// back to synchronous dropping at its next safe boundary.
    SummaryFailed { generation: u64 },
}

/// Drive one tool-free summary request to completion and return its text.
pub async fn generate_summary(
    engine: Arc<dyn Engine>,
    request: Vec<Message>,
    session_id: Option<uuid::Uuid>,
) -> anyhow::Result<String> {
    let mut stream = engine
        .chat_with_session_id(request, Vec::new(), CancellationToken::new(), session_id)
        .await?;
    let mut summary = String::new();
    loop {
        match stream.recv().await {
            Some(StreamEvent::Text(text)) => summary.push_str(&text),
            Some(StreamEvent::Done) => break,
            Some(StreamEvent::Cancelled) => {
                anyhow::bail!("compaction summary stream was cancelled");
            }
            Some(StreamEvent::Error(error)) => return Err(error),
            Some(StreamEvent::Thinking(_) | StreamEvent::Delta(_)) => {}
            None => break,
        }
    }
    let summary = summary.trim().to_string();
    anyhow::ensure!(
        !summary.is_empty(),
        "compact engine returned an empty summary"
    );
    Ok(summary)
}

/// Runtime bookkeeping for background compaction inside the outer loop.
///
/// Owns the summary outcome channel plus the in-flight/pending state. The
/// outer loop — the sole writer of [`Context`] — drives it only at safe
/// boundaries (a completed turn with an empty serial tool queue), so a
/// completed summary is applied exactly like a steer at a natural point, and
/// the background task itself only ever sees an immutable prefix clone.
pub(crate) struct CompactionRuntime {
    engine: Option<Arc<dyn Engine>>,
    /// Framing instruction for every summary request: the resolved
    /// `COMPACTION.md`, or its built-in default.
    summary_prompt: String,
    outcome_tx: mpsc::Sender<CompactionOutcome>,
    summary_in_flight: bool,
    pending: Option<CompactionSnapshot>,
    fallback_due: bool,
}

impl CompactionRuntime {
    /// Create the runtime and the receiver the loop polls for summary
    /// outcomes. `engine` is the profile's `compact_engine`; `None` (child
    /// loops, or a profile without one) disables compaction entirely.
    /// `summary_prompt` is the framing instruction the compact engine gets.
    pub(crate) fn new(
        engine: Option<Arc<dyn Engine>>,
        summary_prompt: String,
    ) -> (Self, mpsc::Receiver<CompactionOutcome>) {
        let (outcome_tx, outcome_rx) = mpsc::channel(4);
        (
            Self {
                engine,
                summary_prompt,
                outcome_tx,
                summary_in_flight: false,
                pending: None,
                fallback_due: false,
            },
            outcome_rx,
        )
    }

    /// Whether this loop compacts at all: only root conversations with a
    /// configured compact engine do (blueprint §4C *scope*).
    pub(crate) fn enabled(&self) -> bool {
        self.engine.is_some()
    }

    /// Record one summary attempt's outcome. Results stamped with another
    /// generation — a summary started before `AgentCommand::ClearContext` —
    /// are ignored, so a stale snapshot can never be applied to an unrelated
    /// conversation. The in-flight flag clears regardless: the attempt it
    /// tracks is over either way.
    pub(crate) fn on_outcome(&mut self, outcome: CompactionOutcome, generation: u64) {
        self.summary_in_flight = false;
        match outcome {
            CompactionOutcome::SummaryReady {
                generation: outcome_generation,
                boundary,
                summary,
            } => {
                if outcome_generation != generation {
                    tracing::debug!(
                        outcome_generation,
                        generation,
                        boundary,
                        "dropping compaction summary from a superseded generation"
                    );
                    return;
                }
                self.pending = Some(CompactionSnapshot { boundary, summary });
            }
            CompactionOutcome::SummaryFailed {
                generation: outcome_generation,
            } => {
                if outcome_generation != generation {
                    return;
                }
                self.fallback_due = true;
            }
        }
    }

    /// Forget everything recorded for the conversation `ClearContext` just
    /// replaced; a summary computed for the old transcript must never be
    /// applied to the new one.
    pub(crate) fn reset(&mut self) {
        self.summary_in_flight = false;
        self.pending = None;
        self.fallback_due = false;
    }

    /// Apply whatever finished, honor a forced fallback, then evaluate the
    /// thresholds for the current model view.
    ///
    /// Called only at safe boundaries: a completed turn or a freshly arrived
    /// user message while idle, always with an empty serial tool queue.
    pub(crate) fn sweep(&mut self, context: &mut Context, engine: &dyn Engine, generation: u64) {
        if !self.enabled() {
            return;
        }
        let Some(budget) = CompactionBudget::from_engine(engine) else {
            return;
        };
        let context_length = budget.context_length;
        if let Some(snapshot) = self.pending.take() {
            let boundary = snapshot.boundary;
            let summary = snapshot.summary.clone();
            let tokens_before = context.model_view_token_count();
            let messages_before = context.model_view_message_count();
            if context.set_compaction(snapshot) {
                let effectiveness = CompactionEffectiveness::measure(
                    tokens_before,
                    context.model_view_token_count(),
                    messages_before,
                    context.model_view_message_count(),
                );
                // Persist the snapshot itself; the transcript it summarizes is
                // already on disk and stays untouched (append-only).
                if let Some(store) = context.store() {
                    // The event carries both coordinates: the live boundary
                    // the model view uses, and the same cut without the
                    // rebuilt system prompts for replay-only readers.
                    store.record_compaction(boundary, context.leading_system_len(), &summary);
                }
                tracing::info!(
                    boundary,
                    summarized_messages = boundary.saturating_sub(context.leading_system_len()),
                    estimated_tokens_before = effectiveness.estimated_tokens_before,
                    estimated_tokens_after = effectiveness.estimated_tokens_after,
                    estimated_token_savings = effectiveness.estimated_token_savings,
                    estimated_reduction_percent = effectiveness.estimated_reduction_percent,
                    model_view_messages_before = effectiveness.model_view_messages_before,
                    model_view_messages_after = effectiveness.model_view_messages_after,
                    model_view_messages_removed = effectiveness.removed_model_view_messages(),
                    "compaction summary applied at a safe boundary"
                );
            }
        }
        let target_tokens = budget.target_tokens();
        if self.fallback_due {
            self.fallback_due = false;
            let tokens_before = context.model_view_token_count();
            let messages_before = context.model_view_message_count();
            let dropped = context.drop_model_view_to(target_tokens);
            let effectiveness = CompactionEffectiveness::measure(
                tokens_before,
                context.model_view_token_count(),
                messages_before,
                context.model_view_message_count(),
            );
            tracing::warn!(
                dropped,
                context_length,
                target_tokens,
                estimated_tokens_before = effectiveness.estimated_tokens_before,
                estimated_tokens_after = effectiveness.estimated_tokens_after,
                estimated_token_savings = effectiveness.estimated_token_savings,
                estimated_reduction_percent = effectiveness.estimated_reduction_percent,
                model_view_messages_before = effectiveness.model_view_messages_before,
                model_view_messages_after = effectiveness.model_view_messages_after,
                model_view_messages_removed = effectiveness.removed_model_view_messages(),
                "compaction summary failed; dropped the model view prefix synchronously"
            );
        }
        let usage_ratio = budget.usage_ratio(context);
        let action = plan_compaction(
            usage_ratio,
            self.summary_in_flight,
            budget.soft_threshold,
            budget.hard_threshold,
        );
        tracing::debug!(
            context_length,
            estimated_tokens = context.model_view_token_count(),
            model_view_messages = context.model_view_message_count(),
            usage_ratio,
            target_tokens,
            soft_threshold = budget.soft_threshold,
            hard_threshold = budget.hard_threshold,
            summary_in_flight = self.summary_in_flight,
            action = ?action,
            "compaction sweep evaluated"
        );
        match action {
            CompactionAction::None => {}
            CompactionAction::StartSummary => {
                self.start_summary(context, context_length, target_tokens, generation);
            }
            CompactionAction::DropFallback => {
                let tokens_before = context.model_view_token_count();
                let messages_before = context.model_view_message_count();
                let dropped = context.drop_model_view_to(target_tokens);
                let effectiveness = CompactionEffectiveness::measure(
                    tokens_before,
                    context.model_view_token_count(),
                    messages_before,
                    context.model_view_message_count(),
                );
                tracing::warn!(
                    usage_ratio,
                    dropped,
                    context_length,
                    target_tokens,
                    estimated_tokens_before = effectiveness.estimated_tokens_before,
                    estimated_tokens_after = effectiveness.estimated_tokens_after,
                    estimated_token_savings = effectiveness.estimated_token_savings,
                    estimated_reduction_percent = effectiveness.estimated_reduction_percent,
                    model_view_messages_before = effectiveness.model_view_messages_before,
                    model_view_messages_after = effectiveness.model_view_messages_after,
                    model_view_messages_removed = effectiveness.removed_model_view_messages(),
                    "model view reached the hard compaction threshold; \
                     dropped the prefix synchronously"
                );
            }
        }
    }

    /// Snapshot the oldest prefix worth summarizing, then drive the compact
    /// engine over it in a background task. The task only ever sees its own
    /// clone of the prefix and reports back through the outcome channel.
    fn start_summary(
        &mut self,
        context: &Context,
        context_length: usize,
        target_tokens: usize,
        generation: u64,
    ) {
        if self.summary_in_flight {
            return;
        }
        let Some(engine) = self.engine.clone() else {
            return;
        };
        let Some(boundary) = context.summary_boundary_for_target(target_tokens) else {
            return;
        };
        let leading_system_len = context.leading_system_len();
        let prefix = context.messages()[leading_system_len..boundary].to_vec();
        let request = build_summary_request(&prefix, &self.summary_prompt);
        let session_id = context.store().map(|store| store.session_id());
        self.summary_in_flight = true;
        let outcome_tx = self.outcome_tx.clone();
        tracing::info!(
            boundary,
            prefix_messages = prefix.len(),
            context_length,
            "background compaction summary started"
        );
        tokio::spawn(async move {
            let outcome = match tokio::time::timeout(
                COMPACTION_SUMMARY_TIMEOUT,
                generate_summary(engine, request, session_id),
            )
            .await
            {
                Ok(Ok(summary)) => CompactionOutcome::SummaryReady {
                    generation,
                    boundary,
                    summary,
                },
                Ok(Err(error)) => {
                    tracing::warn!(%error, boundary, "compaction summary failed");
                    CompactionOutcome::SummaryFailed { generation }
                }
                Err(_elapsed) => {
                    tracing::warn!(
                        boundary,
                        timeout_secs = COMPACTION_SUMMARY_TIMEOUT.as_secs(),
                        "compaction summary timed out"
                    );
                    CompactionOutcome::SummaryFailed { generation }
                }
            };
            let _ = outcome_tx.send(outcome).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AssistantStatus, ToolCallRequest, ToolResult};
    use crate::llm::ToolSchema;
    use serde_json::Value;
    use std::sync::Mutex;
    use tokio::sync::mpsc;

    fn user(text: &str) -> Message {
        Message::User {
            content: text.to_string(),
            kind: UserMessageKind::Explicit,
        }
    }

    fn assistant_with_call(id: Uuid) -> Message {
        Message::Assistant {
            content: String::new(),
            status: AssistantStatus::Complete,
            tool_calls: vec![ToolCallRequest {
                id,
                name: "read".to_string(),
                arguments: Value::Null,
                persisted_title_arguments: Vec::new(),
            }],
        }
    }

    fn tool_result(id: Uuid) -> Message {
        Message::ToolResult {
            value: ToolResult {
                id,
                name: "read".to_string(),
                result: "file contents".to_string(),
                attachments: vec![],
                result_section_id: None,
                result_metadata: None,
                result_metadata_schema: None,
                summary: vec![],
                content_sections: vec![],
                duration: Duration::ZERO,
            },
        }
    }

    #[test]
    fn the_budget_combines_the_window_with_the_engines_compaction_knobs() {
        let config = crate::llm::EngineConfig {
            provider_connection: "mock".to_string(),
            model: "test-model".to_string(),
            temperature: None,
            context_length: None,
            api_base_url: None,
            api_key: None,
            reasoning: None,
            compact_target_ratio: 0.4,
            soft_compaction_threshold: 0.8,
            hard_compaction_threshold: 0.95,
        };

        let budget = CompactionBudget::new(10_000, &config).expect("a real window yields a budget");
        assert_eq!(budget.context_length, 10_000);
        assert_eq!(budget.target_ratio, 0.4);
        assert_eq!(budget.soft_threshold, 0.8);
        assert_eq!(budget.hard_threshold, 0.95);
        assert_eq!(budget.target_tokens(), 4_000);

        assert_eq!(
            CompactionBudget::new(0, &config),
            None,
            "a zero-length window has no budget: the sweep must not divide by it"
        );
    }

    #[test]
    fn effectiveness_reports_net_token_savings_and_message_reduction() {
        let summary_shrunk_view = CompactionEffectiveness::measure(1_000, 400, 12, 5);
        assert_eq!(summary_shrunk_view.estimated_token_savings, 600);
        assert_eq!(summary_shrunk_view.estimated_reduction_percent, 60.0);
        assert_eq!(summary_shrunk_view.removed_model_view_messages(), 7);
        assert_eq!(summary_shrunk_view.estimated_tokens_before, 1_000);
        assert_eq!(summary_shrunk_view.estimated_tokens_after, 400);

        let large_summary = CompactionEffectiveness::measure(100, 140, 3, 2);
        assert_eq!(large_summary.estimated_token_savings, -40);
        assert_eq!(large_summary.estimated_reduction_percent, -40.0);
        assert_eq!(large_summary.removed_model_view_messages(), 1);

        let empty_view = CompactionEffectiveness::measure(0, 0, 0, 0);
        assert_eq!(empty_view.estimated_reduction_percent, 0.0);
        assert_eq!(empty_view.removed_model_view_messages(), 0);
    }

    #[test]
    fn summary_application_log_reports_estimated_model_view_reduction() {
        let log = crate::test_support::capture_logs(async {
            let mut context = Context::new();
            context.set_token_estimator(crate::llm::TokenEstimator::FourCharactersPerToken);
            context.push_user("old request ".repeat(80));
            context.push_assistant("old response ".repeat(40), AssistantStatus::Complete);
            let boundary = context.messages().len();
            let context_length = 16_000;
            let config = crate::llm::EngineConfig {
                provider_connection: "mock".to_string(),
                model: "test-model".to_string(),
                temperature: None,
                context_length: Some(context_length),
                api_base_url: None,
                api_key: None,
                reasoning: None,
                compact_target_ratio: 0.5,
                soft_compaction_threshold: 0.7,
                hard_compaction_threshold: 0.9,
            };
            let engine: Arc<dyn Engine> = Arc::new(crate::llm::EngineImpl::new(
                Arc::new(crate::llm::MockProvider::new(vec![], Duration::ZERO)),
                config,
            ));
            let (mut runtime, _outcome_rx) = CompactionRuntime::new(
                Some(Arc::clone(&engine)),
                COMPACTION_SUMMARY_PROMPT.to_string(),
            );
            runtime.pending = Some(CompactionSnapshot {
                boundary,
                summary: "short summary".to_string(),
            });

            let tokens_before = context.model_view_token_count();
            runtime.sweep(&mut context, engine.as_ref(), 0);
            let tokens_after = context.model_view_token_count();
            assert!(tokens_after < tokens_before);
        });

        assert!(log.contains("compaction summary applied at a safe boundary"));
        assert!(log.contains("estimated_tokens_before="));
        assert!(log.contains("estimated_tokens_after="));
        assert!(log.contains("estimated_token_savings="));
        assert!(log.contains("estimated_reduction_percent="));
        assert!(log.contains("model_view_messages_removed="));
    }

    #[test]
    fn synchronous_drop_log_reports_estimated_model_view_reduction() {
        let log = crate::test_support::capture_logs(async {
            let mut context = Context::new();
            context.set_token_estimator(crate::llm::TokenEstimator::FourCharactersPerToken);
            context.push_user("large old turn ".repeat(400));
            context.push_assistant("large old response ".repeat(200), AssistantStatus::Complete);
            let tokens_before = context.model_view_token_count();
            let config = crate::llm::EngineConfig {
                provider_connection: "mock".to_string(),
                model: "test-model".to_string(),
                temperature: None,
                context_length: Some(2_000),
                api_base_url: None,
                api_key: None,
                reasoning: None,
                compact_target_ratio: 0.5,
                soft_compaction_threshold: 0.7,
                hard_compaction_threshold: 0.9,
            };
            let engine: Arc<dyn Engine> = Arc::new(crate::llm::EngineImpl::new(
                Arc::new(crate::llm::MockProvider::new(vec![], Duration::ZERO)),
                config,
            ));
            let (mut runtime, _outcome_rx) = CompactionRuntime::new(
                Some(Arc::clone(&engine)),
                COMPACTION_SUMMARY_PROMPT.to_string(),
            );

            runtime.sweep(&mut context, engine.as_ref(), 0);
            assert!(context.model_view_token_count() < tokens_before);
        });

        assert!(log.contains("model view reached the hard compaction threshold"));
        assert!(log.contains("estimated_tokens_before="));
        assert!(log.contains("estimated_tokens_after="));
        assert!(log.contains("estimated_token_savings="));
        assert!(log.contains("estimated_reduction_percent="));
        assert!(log.contains("model_view_messages_removed="));
    }

    #[test]
    fn no_op_sweep_log_reports_estimate_window_and_decision() {
        let log = crate::test_support::capture_logs(async {
            let mut context = Context::new();
            context.set_token_estimator(crate::llm::TokenEstimator::FourCharactersPerToken);
            context.push_user("small request".to_string());
            let config = crate::llm::EngineConfig {
                provider_connection: "mock".to_string(),
                model: "test-model".to_string(),
                temperature: None,
                context_length: Some(20_000),
                api_base_url: None,
                api_key: None,
                reasoning: None,
                compact_target_ratio: 0.5,
                soft_compaction_threshold: 0.7,
                hard_compaction_threshold: 0.9,
            };
            let engine: Arc<dyn Engine> = Arc::new(crate::llm::EngineImpl::new(
                Arc::new(crate::llm::MockProvider::new(vec![], Duration::ZERO)),
                config,
            ));
            let (mut runtime, _outcome_rx) = CompactionRuntime::new(
                Some(Arc::clone(&engine)),
                COMPACTION_SUMMARY_PROMPT.to_string(),
            );

            runtime.sweep(&mut context, engine.as_ref(), 0);
        });

        assert!(log.contains("compaction sweep evaluated"));
        assert!(log.contains("context_length=20000"));
        assert!(log.contains("estimated_tokens="));
        assert!(log.contains("usage_ratio="));
        assert!(log.contains("target_tokens=10000"));
        assert!(log.contains("soft_threshold=0.7"));
        assert!(log.contains("hard_threshold=0.9"));
        assert!(log.contains("action=None"));
    }

    #[test]
    fn threshold_switching_follows_the_soft_and_hard_triggers() {
        let soft = DEFAULT_SOFT_COMPACTION_THRESHOLD;
        let hard = DEFAULT_HARD_COMPACTION_THRESHOLD;

        assert_eq!(
            plan_compaction(0.69, false, soft, hard),
            CompactionAction::None
        );
        assert_eq!(
            plan_compaction(soft, false, soft, hard),
            CompactionAction::StartSummary
        );
        assert_eq!(
            plan_compaction(0.85, false, soft, hard),
            CompactionAction::StartSummary
        );
        assert_eq!(
            plan_compaction(0.85, true, soft, hard),
            CompactionAction::None,
            "one summary at a time: a second sweep waits for the first"
        );
        assert_eq!(
            plan_compaction(hard + 0.01, true, soft, hard),
            CompactionAction::DropFallback,
            "the hard trigger wins even while a summary is in flight"
        );
        assert_eq!(
            plan_compaction(hard + 0.01, false, soft, hard),
            CompactionAction::DropFallback
        );

        // The triggers are the engine's, not the constants': a later engine
        // can raise them without touching this logic.
        assert_eq!(
            plan_compaction(0.85, false, 0.9, 0.95),
            CompactionAction::None
        );
        assert_eq!(
            plan_compaction(0.9, false, 0.9, 0.95),
            CompactionAction::StartSummary
        );
        assert_eq!(
            plan_compaction(0.96, false, 0.9, 0.95),
            CompactionAction::DropFallback
        );
    }

    #[test]
    fn safe_boundaries_never_split_a_tool_call_from_its_result() {
        let call_id = Uuid::new_v4();
        let other_call_id = Uuid::new_v4();
        let messages = vec![
            user("question"),
            assistant_with_call(call_id),
            tool_result(call_id),
            assistant_with_call(other_call_id),
            tool_result(other_call_id),
            user("follow-up"),
        ];

        let boundaries = safe_boundaries_from(&messages, 0);
        assert_eq!(boundaries, vec![0, 1, 3, 5, 6]);
        for boundary in [3, 5] {
            assert!(
                first_safe_boundary_from(&messages, boundary) == Some(boundary),
                "cutting at {boundary} keeps every call with its result"
            );
        }
        // Cutting between the call and its result must not be offered.
        assert!(!boundaries.contains(&2));
        assert!(!boundaries.contains(&4));
        assert_eq!(first_safe_boundary_from(&messages, 2), Some(3));
    }

    #[test]
    fn an_orphaned_tool_call_only_allows_a_boundary_after_its_synthetic_result() {
        let orphan = Uuid::new_v4();
        let messages = vec![
            user("question"),
            assistant_with_call(orphan),
            // `Context::hydrate` appends this synthetic error for a call that
            // never completed before the session ended.
            Message::ToolError {
                id: orphan,
                name: "read".to_string(),
                error: "Tool call did not complete before the session ended.".to_string(),
                duration: Duration::ZERO,
                content_sections: vec![],
            },
            user("next turn"),
        ];

        // Cutting before the call keeps call and result together in the
        // tail; cutting between them is what must be refused.
        assert_eq!(safe_boundaries_from(&messages, 0), vec![0, 1, 3, 4]);
        assert_eq!(first_safe_boundary_from(&messages, 2), Some(3));
        assert_eq!(first_safe_boundary_from(&messages, 3), Some(3));

        // A call that never gets a result leaves no safe cut behind it.
        let unresolved = vec![user("question"), assistant_with_call(orphan)];
        assert_eq!(first_safe_boundary_from(&unresolved, 2), None);
    }

    #[test]
    fn the_summary_request_is_tool_free_and_framed_by_the_prompt() {
        let prefix = vec![user("do the thing"), assistant_with_call(Uuid::new_v4())];

        let request = build_summary_request(&prefix, COMPACTION_SUMMARY_PROMPT);

        assert!(matches!(
            request.first(),
            Some(Message::System { content }) if content == COMPACTION_SUMMARY_PROMPT
        ));

        // A loaded `COMPACTION.md` replaces only the framing instruction; the
        // snapshot and the trailing instruction stay exactly as they were.
        let customized = build_summary_request(&prefix, "Keep the run identifiers.");
        assert!(matches!(
            customized.first(),
            Some(Message::System { content }) if content == "Keep the run identifiers."
        ));
        assert_eq!(&customized[1..customized.len() - 1], prefix.as_slice());
        assert_eq!(&request[1..request.len() - 1], prefix.as_slice());
        assert!(
            matches!(
                request.last(),
                Some(Message::User { content, .. }) if content == "Write the summary now."
            ),
            "the request ends with an instruction, even when the prefix ends in a tool call: {request:?}"
        );
    }

    /// One request as the recording provider saw it.
    #[derive(Clone)]
    struct RecordedRequest {
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
    }

    /// Records every request it receives and answers with a canned summary.
    struct RecordingSummaryProvider {
        recorded: Arc<Mutex<Vec<RecordedRequest>>>,
        reply: String,
    }

    impl crate::llm::Provider for RecordingSummaryProvider {
        fn name(&self) -> &str {
            "recording-summary"
        }

        fn list_models(
            &self,
        ) -> futures::future::BoxFuture<'_, anyhow::Result<Vec<crate::llm::ModelInfo>>> {
            Box::pin(async { Ok(vec![]) })
        }

        fn chat<'a>(
            &'a self,
            messages: Vec<Message>,
            tools: Vec<ToolSchema>,
            _cancel: CancellationToken,
        ) -> futures::future::BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
            self.recorded
                .lock()
                .expect("recording mutex is not poisoned")
                .push(RecordedRequest { messages, tools });
            let reply = self.reply.clone();
            Box::pin(async move {
                let (tx, rx) = mpsc::channel(4);
                tokio::spawn(async move {
                    let _ = tx.send(StreamEvent::Text(reply)).await;
                    let _ = tx.send(StreamEvent::Done).await;
                });
                Ok(rx)
            })
        }
    }

    #[tokio::test]
    async fn generate_summary_collects_the_text_without_tools() {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let engine: Arc<dyn Engine> = Arc::new(crate::llm::EngineImpl::new(
            Arc::new(RecordingSummaryProvider {
                recorded: Arc::clone(&recorded),
                reply: "  A concise summary.  ".to_string(),
            }),
            crate::llm::EngineConfig {
                provider_connection: "mock".to_string(),
                model: "summary-model".to_string(),
                temperature: None,
                context_length: None,
                api_base_url: None,
                api_key: None,
                reasoning: None,
                compact_target_ratio: 0.5,
                soft_compaction_threshold: 0.7,
                hard_compaction_threshold: 0.9,
            },
        ));
        let prefix = vec![user("do the thing")];

        let summary = generate_summary(
            engine,
            build_summary_request(&prefix, COMPACTION_SUMMARY_PROMPT),
            None,
        )
        .await
        .expect("summary generated");

        assert_eq!(summary, "A concise summary.");
        let requests = recorded
            .lock()
            .expect("recording mutex is not poisoned")
            .clone();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert!(
            request.tools.is_empty(),
            "the summarizer is tool-free: {:?}",
            request.tools
        );
        assert_eq!(
            &request.messages[1..request.messages.len() - 1],
            prefix.as_slice()
        );
    }

    #[tokio::test]
    async fn an_empty_summary_response_is_a_failure() {
        struct EmptyProvider;

        impl crate::llm::Provider for EmptyProvider {
            fn name(&self) -> &str {
                "empty"
            }

            fn list_models(
                &self,
            ) -> futures::future::BoxFuture<'_, anyhow::Result<Vec<crate::llm::ModelInfo>>>
            {
                Box::pin(async { Ok(vec![]) })
            }

            fn chat<'a>(
                &'a self,
                _messages: Vec<Message>,
                _tools: Vec<ToolSchema>,
                _cancel: CancellationToken,
            ) -> futures::future::BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>>
            {
                Box::pin(async move {
                    let (tx, rx) = mpsc::channel(4);
                    tokio::spawn(async move {
                        let _ = tx.send(StreamEvent::Text("   \n".to_string())).await;
                        let _ = tx.send(StreamEvent::Done).await;
                    });
                    Ok(rx)
                })
            }
        }

        let engine: Arc<dyn Engine> = Arc::new(crate::llm::EngineImpl::new(
            Arc::new(EmptyProvider),
            crate::llm::EngineConfig {
                provider_connection: "mock".to_string(),
                model: "summary-model".to_string(),
                temperature: None,
                context_length: None,
                api_base_url: None,
                api_key: None,
                reasoning: None,
                compact_target_ratio: 0.5,
                soft_compaction_threshold: 0.7,
                hard_compaction_threshold: 0.9,
            },
        ));

        let error = generate_summary(
            engine,
            build_summary_request(&[user("hello")], COMPACTION_SUMMARY_PROMPT),
            None,
        )
        .await
        .expect_err("an empty summary must fail so the fallback runs");
        assert!(error.to_string().contains("empty summary"), "{error}");
    }

    #[test]
    fn the_summary_marker_projects_a_user_message_never_a_system_message() {
        let snapshot = CompactionSnapshot {
            boundary: 3,
            summary: "decided to use the store".to_string(),
        };

        let message = snapshot.summary_message();
        assert!(matches!(
            &message,
            Message::User { content, kind } if content.starts_with(COMPACTION_SUMMARY_MARKER)
                && content.ends_with("decided to use the store")
                && *kind == UserMessageKind::ImplicitToolNote
        ));
    }
}
