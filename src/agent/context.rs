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

use super::{
    compaction, tools, AssistantStatus, Message, ToolCallRequest, ToolResult, UserMessageKind,
    MAX_EXECUTED_TOOL_CALLS_PER_BATCH, MINIMUM_USEFUL_RESULT_TOKENS, TOOL_PROTOCOL_SYSTEM_PROMPT,
};
use crate::llm::engine::Engine;
use crate::llm::{ToolEnvelope, ToolSchema};
use crate::prompt::{format_prompt_block, PromptBlockKind};
use crate::store::Store;
use std::collections::HashMap;
use std::sync::Mutex;
use uuid::Uuid;
mod projection;
mod recovery;

/// Estimated framing allowance per message and per embedded tool call.
const TOKEN_FRAMING_OVERHEAD: usize = 8;

/// Priority tier of a message in the model-facing view.
///
/// Tiers describe how the *model view* treats a message when the context
/// window fills. The transcript itself is tier-agnostic: it keeps every
/// message regardless of what the model view later drops or summarizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageTier {
    /// Tier 0: the core identity prompt — never dropped or summarized.
    System,
    /// Tier 1: stable instructions (tool protocol, persona, briefing) —
    /// never dropped initially.
    InitialContext,
    /// Tier 2: ordinary conversation turns — dropped last.
    Recent,
    /// Tier 3: previously summarized buckets — dropped before `Recent`.
    Compacted,
}

/// Bookkeeping cached for one transcript message when it enters the context:
/// its tier in the model-facing view and its approximate token count.
///
/// Held index-parallel to [`Context::messages`] so the transcript stays a plain
/// `Vec<Message>` for replay, persistence and cloning, and so no message text
/// is stored twice.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ContextEntry {
    tier: MessageTier,
    token_count: usize,
}

/// The tier a message occupies at `index`: the leading identity prompt is
/// the `System` tier, every other system message (tool protocol, persona,
/// briefing, child budget) is stable `InitialContext` instructions, and
/// everything else is conversation `Recent`. [`MessageTier::Compacted`] is
/// only ever assigned to the synthetic summary entry compaction projects
/// into the model view, never to a transcript message.
fn tier_for(index: usize, message: &Message) -> MessageTier {
    match message {
        Message::System { .. } if index == 0 => MessageTier::System,
        Message::System { .. } => MessageTier::InitialContext,
        _ => MessageTier::Recent,
    }
}

/// Approximate token count of one message's model-visible text.
///
/// Counts payload text, result envelopes, and [`TOKEN_FRAMING_OVERHEAD`] for
/// each message and embedded tool call; provider-specific framing is approximate.
/// Request budgeting separately measures the projected model view and tools
/// with reserved response space and safety margins.
pub(crate) fn estimate_message_tokens(
    estimator: crate::llm::TokenEstimator,
    message: &Message,
) -> usize {
    let payload = match message {
        Message::System { content } => estimator.estimate_tokens(content),
        Message::User { content, .. } => estimator.estimate_tokens(content),
        Message::Assistant {
            content,
            tool_calls,
            ..
        } => {
            let mut tokens = estimator.estimate_tokens(content);
            for tool_call in tool_calls {
                tokens += estimator.estimate_tokens(&tool_call.name);
                tokens +=
                    estimator.estimate_tokens(&tool_call.id.to_string()) + TOKEN_FRAMING_OVERHEAD;
                tokens += estimator.estimate_tokens(&tool_call.arguments.to_string());
            }
            tokens
        }
        Message::ToolResult { value } => {
            estimator.estimate_tokens(&value.name)
                + estimator.estimate_tokens(&value.id.to_string())
                + estimator.estimate_tokens(
                    &ToolEnvelope {
                        ok: true,
                        result: Some(value.result.clone()),
                        error: None,
                        result_metadata: value.result_metadata.clone(),
                        result_metadata_schema: value.result_metadata_schema.clone(),
                    }
                    .to_string_checked(),
                )
        }
        Message::ToolError {
            id, name, error, ..
        } => {
            estimator.estimate_tokens(name)
                + estimator.estimate_tokens(&id.to_string())
                + estimator.estimate_tokens(
                    &ToolEnvelope {
                        ok: false,
                        result: None,
                        error: Some(error.clone()),
                        result_metadata: None,
                        result_metadata_schema: None,
                    }
                    .to_string_checked(),
                )
        }
    };
    payload + TOKEN_FRAMING_OVERHEAD
}

pub(crate) fn complete_result_fits(
    id: uuid::Uuid,
    name: &str,
    result: &str,
    metadata: Option<serde_json::Value>,
    metadata_schema: Option<serde_json::Value>,
    allowance: usize,
) -> bool {
    let estimator = crate::llm::TokenEstimator::Tiktoken;
    let envelope = ToolEnvelope {
        ok: true,
        result: Some(result.into()),
        error: None,
        result_metadata: metadata,
        result_metadata_schema: metadata_schema,
    }
    .to_string_checked();
    estimator.estimate_tokens(name)
        + estimator.estimate_tokens(&id.to_string())
        + estimator.estimate_tokens(&envelope)
        + TOKEN_FRAMING_OVERHEAD
        <= allowance
}

#[cfg(test)]
pub(super) fn tool_schema_token_count(tools: &[ToolSchema]) -> usize {
    let estimator = crate::llm::TokenEstimator::Tiktoken;
    tools
        .iter()
        .map(|tool| {
            estimator.estimate_tokens(&tool.name)
                + estimator.estimate_tokens(&tool.description)
                + estimator.estimate_tokens(&tool.parameters.to_string())
        })
        .sum()
}

#[derive(Debug, Default)]
struct ToolSchemaTokenCache {
    inputs: Vec<String>,
    token_count: usize,
}

impl ToolSchemaTokenCache {
    fn count(&mut self, tools: &[ToolSchema]) -> usize {
        let inputs: Vec<String> = tools
            .iter()
            .flat_map(|tool| {
                [
                    tool.name.clone(),
                    tool.description.clone(),
                    tool.parameters.to_string(),
                ]
            })
            .collect();
        if self.inputs != inputs {
            self.token_count = inputs
                .iter()
                .map(|text| crate::llm::TokenEstimator::Tiktoken.estimate_tokens(text))
                .sum();
            self.inputs = inputs;
        }
        self.token_count
    }
}

/// The conversation as the agent loop records it: every committed message, in
/// order, exactly as replayed by the TUI and persisted to the session file.
///
/// The model-facing view is built separately (see [`Context::model_messages`])
/// so compaction can replace an old prefix without touching this record.
#[derive(Debug, Default)]
pub struct Context {
    messages: Vec<Message>,
    /// Bookkeeping for `messages`, index-aligned and maintained exclusively
    /// by [`Context::push_entry`] (i.e. by `commit` and `hydrate`): tier plus
    /// the token count cached when the message entered the context.
    entries: Vec<ContextEntry>,
    /// Token estimator used to (re)build `entries`' counts. Chosen from the
    /// active engine's provider connection and refreshed when the model
    /// switches.
    estimator: crate::llm::TokenEstimator,
    tool_schema_token_cache: Mutex<ToolSchemaTokenCache>,
    /// The latest applied compaction: `messages[..boundary]` is represented
    /// in the model view by its marked summary. The transcript keeps every
    /// summarized message untouched.
    compaction: Option<crate::agent::compaction::CompactionSnapshot>,
    /// First transcript index included in the model view after the leading
    /// system block. `0` means "nothing dropped"; [`Context::drop_model_view_to`]
    /// and [`Context::set_compaction`] advance it, never rewind it. Like
    /// `compaction`, this shapes the model view only — the transcript is
    /// never rewritten.
    view_start: usize,
    /// Handoff and task anchor standing in for content a lossy recovery
    /// removed from the model view; superseded by a summary that covers its
    /// boundary.
    recovery: Option<crate::agent::compaction::recovery::RecoveryProjection>,
    model_overrides: HashMap<usize, Message>,
    summary_override: Option<String>,
    retry_after_context_rejection: bool,
    projection_revision: u64,
    projection_dirty: bool,
    compaction_request_id: Option<Uuid>,
    compaction_trigger: &'static str,
    batch_admitted_calls: usize,
    batch_skipped_calls: usize,
    /// Session store this context appends its committed messages to, when
    /// persistence is enabled. `None` for child agents and
    /// for runs started without a store.
    store: Option<Store>,
}

impl Context {
    pub(super) fn retry_after_context_rejection(&mut self) {
        self.retry_after_context_rejection = true;
    }

    pub(super) fn take_context_rejection_retry(&mut self) -> bool {
        std::mem::take(&mut self.retry_after_context_rejection)
    }

    pub(super) fn record_batch_admission(&mut self, requested: usize, unknown: usize) {
        self.batch_admitted_calls = requested.saturating_sub(unknown);
        self.batch_skipped_calls = unknown;
    }

    pub(super) fn record_skipped_call(&mut self) {
        self.batch_admitted_calls = self.batch_admitted_calls.saturating_sub(1);
        self.batch_skipped_calls += 1;
    }

    pub(super) fn set_recovery_trigger(&mut self, retrying: bool) {
        self.compaction_trigger = if retrying {
            "provider_context_rejection"
        } else {
            "request_boundary"
        };
    }

    pub(super) fn set_compaction_diagnostics(
        &mut self,
        request_id: Option<Uuid>,
        trigger: &'static str,
    ) {
        self.compaction_request_id = request_id;
        self.compaction_trigger = trigger;
    }

    pub(super) fn compaction_request_id(&self) -> Option<Uuid> {
        self.compaction_request_id
    }

    pub(super) fn batch_admitted_calls(&self) -> usize {
        self.batch_admitted_calls
    }

    pub(super) fn batch_skipped_calls(&self) -> usize {
        self.batch_skipped_calls
    }

    pub fn new() -> Self {
        Self::with_system_prompt(crate::config::soul::DEFAULT_SOUL)
    }

    /// Construct a context with a caller-supplied initial system message.
    pub fn with_system_prompt(system_prompt: impl Into<String>) -> Self {
        let mut context = Context {
            store: None,
            entries: Vec::new(),
            estimator: crate::llm::TokenEstimator::default(),
            tool_schema_token_cache: Mutex::default(),
            compaction: None,
            view_start: 0,
            recovery: None,
            model_overrides: HashMap::new(),
            summary_override: None,
            retry_after_context_rejection: false,
            projection_revision: 0,
            projection_dirty: false,
            compaction_request_id: None,
            compaction_trigger: "request_boundary",
            batch_admitted_calls: 0,
            batch_skipped_calls: 0,
            messages: Vec::new(),
        };
        context.push_entry(Message::System {
            content: system_prompt.into(),
        });
        context.push_entry(Message::System {
            content: TOOL_PROTOCOL_SYSTEM_PROMPT.to_string(),
        });
        context
    }

    /// Append a message and its bookkeeping together, without forwarding to
    /// the session store — hydration replays messages the store already
    /// holds, while [`Context::commit`] persists first and then lands here.
    fn push_entry(&mut self, message: Message) {
        let index = self.messages.len();
        let entry = ContextEntry {
            tier: tier_for(index, &message),
            token_count: estimate_message_tokens(self.estimator, &message),
        };
        self.messages.push(message);
        self.entries.push(entry);
        debug_assert_eq!(
            self.messages.len(),
            self.entries.len(),
            "transcript and bookkeeping stay index-aligned"
        );
    }

    /// Attach a session store, so every message committed from here on is
    /// also appended to the session file.
    ///
    /// The two system prompts installed by [`Context::new`] or
    /// [`Context::with_system_prompt`] are deliberately not persisted: they
    /// are rebuilt from the running binary on every resume, so storing them
    /// would pin a stale copy into the transcript.
    pub fn set_store(&mut self, store: Store) {
        match Store::read_session_state(store.path()) {
            Ok(state) => {
                self.hydrate(state.messages);
                // Rebuild the model view from the latest snapshot plus the
                // verbatim tail; the transcript above already carries every
                // original message, and re-applying is a read — it never
                // writes the event back.
                if let Some(mut snapshot) = state.compaction {
                    if let Some(boundary) = state.compaction_transcript_boundary {
                        snapshot.boundary = boundary + self.leading_system_len();
                    }
                    self.set_compaction(snapshot);
                }
                if let Some(projection) = state.model_projection {
                    self.restore_model_projection(projection);
                }
            }
            Err(error) => tracing::warn!(
                %error,
                path = %store.path().display(),
                "could not replay persisted messages"
            ),
        }
        self.store = Some(store);
    }

    /// Load committed messages into this context without writing them back.
    /// Existing context messages remain at the front; persisted messages are
    /// appended in store order. Malformed store lines are handled by the store's
    /// tolerant read path.
    pub fn hydrate(&mut self, messages: Vec<Message>) {
        for message in messages {
            self.push_entry(message);
        }

        let mut completed_tool_calls = std::collections::HashSet::new();
        let mut tool_calls = Vec::new();
        for message in &self.messages {
            match message {
                Message::Assistant {
                    tool_calls: calls, ..
                } => {
                    tool_calls.extend(calls.iter().map(|call| (call.id, call.name.clone())));
                }
                Message::ToolResult { value } => {
                    completed_tool_calls.insert(value.id);
                }
                Message::ToolError { id, .. } => {
                    completed_tool_calls.insert(*id);
                }
                _ => {}
            }
        }
        for (id, name) in tool_calls {
            if !completed_tool_calls.contains(&id)
                && name != tools::spawn_subagent::SpawnSubagentTool::NAME
            {
                self.push_entry(Message::ToolError {
                    id,
                    name,
                    error: "Tool call did not complete before the session ended.".to_string(),
                    duration: std::time::Duration::ZERO,
                    content_sections: vec![],
                });
            }
        }
    }

    /// The session store this context currently appends to, when persistence
    /// is enabled. Cloning is cheap (see [`Store`]). Tracks `/clear`
    /// rollovers correctly — unlike a store handle captured once at loop
    /// startup — because it reads whichever store `set_store` last attached.
    pub fn store(&self) -> Option<Store> {
        self.store.clone()
    }

    /// The single commit point for the conversation: every message enters the
    /// context here, and is forwarded to the session store from here.
    ///
    /// Keeping the forward in one place upholds the ownership
    /// rule — only the outer loop mutates `Context`, so only the outer loop
    /// can produce a store event, and the on-disk order is the commit order.
    fn commit(&mut self, message: Message) {
        if let Some(store) = &self.store {
            store.record_message(&message);
        }
        self.push_entry(message);
    }

    pub fn push_user(&mut self, text: String) {
        self.commit(Message::User {
            content: text,
            kind: UserMessageKind::Explicit,
        });
    }

    /// Push an additional system message. Used to inject a `Role::Child`
    /// agent's `briefing` ahead of its task, after the standard system
    /// prompts `Context::new` already installed.
    pub fn push_system(&mut self, text: String) {
        self.commit(Message::System { content: text });
    }

    /// Push a model hint injected by the agent harness. The message is sent
    /// to the model as a regular `role: "user"` turn but is never rendered in
    /// any UI (TUI conversation list or headless CLI output).
    pub fn push_implicit_user(&mut self, text: String) {
        self.commit(Message::User {
            content: text,
            kind: UserMessageKind::ImplicitToolNote,
        });
    }

    /// Push an asynchronous, out-of-band harness notice (e.g. "a spawned
    /// subagent finished/was cancelled") that arrives mid-conversation, well
    /// after the leading system-prompt block. Sent as `push_implicit_user`
    /// (`role:"user"`, hidden from the UI) rather than `push_system`, and
    /// wrapped as a harness-notice block so the model can tell it apart
    /// from a real user message — see that constant's doc comment for why
    /// `role:"system"` is unsafe here.
    pub fn push_async_harness_notice(&mut self, text: String) {
        self.push_implicit_user(format_prompt_block(
            PromptBlockKind::HarnessNotice,
            &format!("Harness notice; not from the user.\n{text}"),
        ));
    }

    pub fn push_assistant(&mut self, content: String, status: AssistantStatus) {
        self.commit(Message::Assistant {
            content,
            status,
            tool_calls: Vec::new(),
        });
    }

    pub fn push_assistant_tool_call(
        &mut self,
        content: String,
        status: AssistantStatus,
        tool_calls: Vec<ToolCallRequest>,
    ) {
        self.commit(Message::Assistant {
            content,
            status,
            tool_calls,
        });
    }

    pub fn push_tool_result(&mut self, value: ToolResult) {
        self.commit(Message::ToolResult { value });
    }

    pub fn push_tool_error(
        &mut self,
        id: Uuid,
        name: String,
        error: String,
        partial_output: Option<&str>,
        duration: std::time::Duration,
        content_sections: Vec<crate::agent::tools::Section>,
    ) {
        let llm_error = match partial_output.filter(|s| !s.is_empty()) {
            Some(output) => format!(
                "{error}\n\nThe tool produced the following output before being terminated:\n\
                 {output}"
            ),
            None => error,
        };
        self.commit(Message::ToolError {
            id,
            name,
            error: llm_error,
            duration,
            content_sections,
        });
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Number of leading contiguous system messages: the rebuilt identity
    /// prompt and tool-protocol prompt (plus any instructions injected ahead
    /// of the first user turn). Compaction neither summarizes nor drops them.
    pub fn leading_system_len(&self) -> usize {
        self.messages
            .iter()
            .take_while(|message| matches!(message, Message::System { .. }))
            .count()
    }

    /// Tier of the message at `index` in the model-facing view.
    pub fn tier(&self, index: usize) -> Option<MessageTier> {
        self.entries.get(index).map(|entry| entry.tier)
    }

    /// The token count cached when the message at `index` entered the
    /// context, in the estimator active at that time.
    pub fn token_count(&self, index: usize) -> Option<usize> {
        self.entries.get(index).map(|entry| entry.token_count)
    }

    /// Sum of every cached token count: the approximate size of the whole
    /// transcript, in tokens. The model view can be smaller — see
    /// [`Context::model_view_token_count`].
    pub fn approximate_token_count(&self) -> usize {
        self.entries.iter().map(|entry| entry.token_count).sum()
    }

    /// Approximate model-view size including the active tool definitions, but
    /// excluding API/JSON transport framing.
    pub fn estimated_model_tokens(&self, tools: &[ToolSchema], _engine: &dyn Engine) -> usize {
        self.model_view_token_count() + self.tool_schema_token_count(tools)
    }

    pub(super) fn tool_schema_token_count(&self, tools: &[ToolSchema]) -> usize {
        self.tool_schema_token_cache
            .lock()
            .expect("tool schema token cache poisoned")
            .count(tools)
    }

    /// Approximate tokens available for one additional model-facing result
    /// while reserving space for a normal reply and compaction safety margin.
    pub fn available_result_token_budget(
        &self,
        tools: &[ToolSchema],
        context_length: usize,
    ) -> usize {
        let current_tokens = self.model_view_token_count() + self.tool_schema_token_count(tools);
        crate::agent::compaction::safe_request_ceiling(context_length)
            .saturating_sub(current_tokens)
    }

    /// Approximate payload-only size of the model view. Retained for callers
    /// that need transcript message accounting.
    pub fn model_view_token_count(&self) -> usize {
        self.model_view_token_count_from(self.model_view_start())
    }

    fn model_view_token_count_from(&self, view_start: usize) -> usize {
        let tail_tokens: usize = self.entries[view_start..]
            .iter()
            .enumerate()
            .map(|(offset, entry)| {
                self.model_overrides
                    .get(&(view_start + offset))
                    .map_or(entry.token_count, |message| {
                        estimate_message_tokens(self.estimator, message)
                    })
            })
            .sum();
        self.model_view_head_tokens() + tail_tokens
    }

    /// Number of messages projected into the current model view, including
    /// the leading system block and the synthetic summary message, if any.
    pub(crate) fn model_view_message_count(&self) -> usize {
        self.leading_system_len()
            + usize::from(self.compaction.is_some())
            + usize::from(self.projected_task_anchor().is_some())
            + self
                .recovery
                .as_ref()
                .map_or(0, |recovery| 1 + usize::from(recovery.anchor.is_some()))
            + self.messages.len().saturating_sub(self.model_view_start())
    }

    /// Approximate share (`0.0`–`1.0+`) of the supplied context window the
    /// model view occupies. `context_length` is the active engine's effective
    /// window ([`crate::llm::Engine::effective_context_length`]), so a
    /// `/models` switch re-evaluates every threshold derived from this.
    pub fn estimated_usage_ratio(&self, context_length: usize) -> f64 {
        if context_length == 0 {
            return 0.0;
        }
        self.model_view_token_count() as f64 / context_length as f64
    }

    /// The latest applied compaction snapshot, if any.
    pub fn compaction(&self) -> Option<&crate::agent::compaction::CompactionSnapshot> {
        self.compaction.as_ref()
    }

    /// Install a prefix summary as the model view's compaction snapshot:
    /// `messages[..boundary]` is replaced by its marked summary, the tail
    /// after `boundary` stays verbatim, and the transcript keeps everything.
    ///
    /// Returns `false` when the snapshot does not advance past the boundary
    /// already applied — boundaries only ever move forward, so an older (or
    /// replayed) result can never resurrect an already summarized prefix.
    pub fn set_compaction(
        &mut self,
        snapshot: crate::agent::compaction::CompactionSnapshot,
    ) -> bool {
        let leading_system_len = self.leading_system_len();
        let clamped_boundary = snapshot
            .boundary
            .clamp(leading_system_len, self.messages.len());
        // Snap forward to a complete-turn cut: a persisted boundary must
        // never re-split a tool call from its one result after a resume.
        let Some(boundary) =
            crate::agent::compaction::first_safe_boundary_from(&self.messages, clamped_boundary)
        else {
            return false;
        };
        if boundary < self.view_start {
            return false;
        }
        if let Some(current) = &self.compaction {
            if boundary <= current.boundary {
                return false;
            }
        }
        self.summary_override = None;
        self.model_overrides.retain(|index, _| *index >= boundary);
        tracing::debug!(
            boundary,
            summary_chars = snapshot.summary.chars().count(),
            "applying compaction snapshot to the model view"
        );
        self.view_start = self.view_start.max(boundary);
        if self
            .recovery
            .as_ref()
            .is_some_and(|recovery| boundary >= recovery.boundary)
        {
            self.recovery = None;
        }
        self.compaction = Some(crate::agent::compaction::CompactionSnapshot {
            boundary,
            summary: snapshot.summary,
        });
        self.projection_dirty = true;
        true
    }

    /// The estimator used to cache token counts.
    pub fn token_estimator(&self) -> crate::llm::TokenEstimator {
        self.estimator
    }

    /// Switch to another estimator (a different provider connection) and
    /// refresh every cached count in one pass. A no-op for the estimator
    /// already in use, so repeated model switches on the same connection
    /// never pay for a recount.
    pub fn set_token_estimator(&mut self, estimator: crate::llm::TokenEstimator) {
        if self.estimator == estimator {
            return;
        }
        self.estimator = estimator;
        for (entry, message) in self.entries.iter_mut().zip(self.messages.iter()) {
            entry.token_count = estimate_message_tokens(estimator, message);
        }
        if let Some(recovery) = &mut self.recovery {
            recovery.token_count = estimate_message_tokens(estimator, &recovery.handoff)
                + recovery
                    .anchor
                    .as_ref()
                    .map_or(0, |message| estimate_message_tokens(estimator, message));
        }
    }
}

#[cfg(test)]
mod tests;
