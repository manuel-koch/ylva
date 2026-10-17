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

use super::runtime::compaction_updates::sweep_compaction_and_report;
use super::*;
pub(super) mod spawn;
use spawn::resolve_spawn_subagent;
/// Log the "tool approval requested" event for the given pending call using the
/// tool's own non-default redacted arguments so sensitive values are never
/// written to the log.
pub(super) fn log_tool_approval_request(call: &dyn ToolCall) {
    let safe_args: serde_json::Map<String, serde_json::Value> =
        call.non_default_redacted_arguments().into_iter().collect();
    tracing::info!(
        tool = %call.name(),
        tool_call_id = %call.id(),
        args = %serde_json::to_string(&safe_args).unwrap_or_else(|_| "{}".to_string()),
        "tool approval requested"
    );
}

/// Whether a tool's model-facing result is subject to the shared per-request
/// context allowance.
pub(super) fn is_result_budgeted(tool_name: &str) -> bool {
    tool_name != tools::agent_finish::AgentFinishTool::NAME
}

/// Per-request token allowance for the result of `tool_name`, or `None` for tools
/// whose results are not context-budgeted.
///
/// The remaining allowance of the request ceiling is reduced by the error
/// framing reserved for every `queued_after` call still waiting, then shared
/// evenly among this call and the budgeted calls queued behind it, so one
/// early result cannot starve the rest of the batch.
pub(super) fn tool_output_token_limit(
    tool_name: &str,
    context: &Context,
    tools: &[ToolSchema],
    context_length: usize,
    queued_after: &[&str],
) -> Option<usize> {
    if !is_result_budgeted(tool_name) {
        return None;
    }
    let available = context
        .available_result_token_budget(tools, context_length)
        .saturating_sub(ERROR_RESPONSE_FRAMING_TOKENS * queued_after.len());
    let unresolved_budgeted = 1 + queued_after
        .iter()
        .filter(|name| is_result_budgeted(name))
        .count();
    Some(available / unresolved_budgeted)
}

pub(super) fn queued_tool_names(pending_tools: &VecDeque<PendingTool>, skip: usize) -> Vec<&str> {
    pending_tools
        .iter()
        .skip(skip)
        .map(|pending| pending.call.name())
        .collect()
}

/// Resolve one requested call that will not be executed: exactly one tool
/// error for the model plus a visible denied bubble for the user.
#[expect(clippy::too_many_arguments)]
pub(super) async fn resolve_skipped_tool_call(
    context: &mut Context,
    ui_tx: &mpsc::Sender<UiUpdate>,
    own_run_id: Option<Uuid>,
    id: Uuid,
    name: String,
    title_arguments: Vec<(String, Value)>,
    content_sections: Vec<crate::agent::tools::Section>,
    reason: String,
) {
    context.record_skipped_call();
    tracing::info!(tool = %name, tool_call_id = %id, %reason, "tool call skipped, not executed");
    context.push_tool_error(
        id,
        name.clone(),
        reason.clone(),
        None,
        std::time::Duration::ZERO,
        content_sections.clone(),
    );
    context.limit_last_model_message(ERROR_RESPONSE_FRAMING_TOKENS);
    let _ = ui_tx
        .send(UiUpdate::ToolAutoDenied {
            id,
            name,
            title_arguments,
            content_sections,
            reason,
            run_id: own_run_id,
        })
        .await;
}

pub(super) fn batch_limit_skip_reason(name: &str, batch_cap: usize) -> String {
    format!(
        "Tool '{name}' was not executed: the response requested more than \
         {batch_cap} tool calls and only the first \
         {batch_cap} are executed. Resubmit only the skipped \
         calls in smaller batches."
    )
}

pub(super) fn context_budget_skip_reason(name: &str) -> String {
    format!(
        "Tool '{name}' was not executed: the remaining context budget cannot hold its \
         result alongside the other pending calls. Resubmit only the skipped calls in \
         smaller batches."
    )
}

/// Advance the pending-tool queue. Processes auto-approved and auto-denied
/// tools immediately in a loop; stops at the first `UserApprovalRequired`
/// tool (sends `ToolPending`) or re-prompts the model when the queue is
/// empty.
///
/// `increment_iterations` is `false` when called immediately after a new
/// `ToolCall` batch arrives (the iteration counter was already set) and
/// `true` after a tool finishes executing (each completed tool round counts).
/// `own_run_id` is the calling loop's own `AgentLoop::own_run_id` (`None` for
/// the root, `Some(run_id)` for a subagent); it is stamped onto the
/// `UiUpdate::ToolPending`/`ClarifyPending` this function may emit so the
/// root (which forwards a subagent's `UiUpdate`s up to its own driver) and the
/// driver can route the resulting `ApproveTool`/`DenyTool`/`AnswerClarify`/
/// `AbortClarify` verdict back to the correct agent loop via
/// `AgentCommand::ForSubagent`. This has no effect on how the tool queue itself
/// is processed — subagents pause and wait exactly like the root; there is no
/// auto-deny special case.
///
/// `pending_subagent_finish_reprompt` is cleared whenever this function
/// re-prompts the model (the queue-empty branch): that re-prompt's `context`
/// already carries any deferred `spawn_subagent` subagent result (see
/// `AgentLoop::run`'s `finish_rx` arm), so the flag's job is done and a later
/// `StreamDone` must not force a redundant extra re-prompt for it.
///
/// `iteration_cap_exceeded` is set (to the guard's explanation message) when
/// the loop's effective `max_tool_iterations` is exceeded — but only for a
/// subagent loop
/// (`own_run_id.is_some()`); a root loop's own hit is reported to the human
/// directly (an `Interrupted` assistant bubble) and never sets this. The
/// caller (`AgentLoop::run`'s `is_subagent && mode == AgentMode::Idle` tail)
/// checks it to return `Err` instead of `Ok(context)` — so
/// `drive_subagent_to_completion` reports a clear `status: "failure"` instead of
/// folding this into the generic "unstructured" fallback used when a subagent
/// simply goes idle without ever calling `agent_finish`. See docs/features.md
/// → *spawn_subagent*/*agent_finish* for the distinction.
/// Increments `*iterations` and, if that now exceeds `max_tool_iterations`
/// (the loop's effective limit; see [`AgentLoop::with_max_tool_iterations`]),
/// reports the guard tripping and forces `*mode = AgentMode::Idle`: for the
/// root (`own_run_id: None`) a human reads the transcript directly, so a real
/// `Interrupted` assistant bubble is pushed; for a subagent, the explanation is
/// instead handed back via `*iteration_cap_exceeded` (never pushed as an
/// assistant message — `extract_last_assistant_text` in `agent_factory.rs`
/// would otherwise mistake this guard boilerplate for the subagent's own final
/// answer) for `AgentLoop::run`'s `is_subagent && mode == AgentMode::Idle` tail
/// to turn into an `Err`, producing an honest `status: "failure"` instead of
/// folding this into the generic "unstructured" fallback (see
/// `advance_tool_queue`'s doc comment and docs/features.md →
/// *spawn_subagent*/*agent_finish*).
///
/// Returns `true` when the cap was exceeded (the caller must not start a new
/// request this round) and `false` otherwise (still under budget, proceed as
/// normal). Shared by `advance_tool_queue`'s queue-empty branch and
/// `AgentLoop::run`'s `finish_rx` idle-reprompt branch — both are re-prompt
/// call sites, and both must enforce the same budget. Without this, a subagent
/// finishing while the root is idle re-prompts it directly via `start_request`,
/// bypassing `advance_tool_queue`'s iteration counter entirely; a model that
/// just keeps calling `spawn_subagent` again on every finish notice (rather
/// than ever answering) can then loop forever — one HTTP request every few
/// seconds, with no cap at all — instead of tripping the same guard as any
/// other tool-use round.
pub(super) async fn bump_and_check_iteration_cap(
    iterations: &mut usize,
    max_tool_iterations: usize,
    context: &mut Context,
    mode: &mut AgentMode,
    ui_tx: &mpsc::Sender<UiUpdate>,
    own_run_id: Option<Uuid>,
    iteration_cap_exceeded: &mut Option<String>,
) -> bool {
    *iterations += 1;
    tracing::debug!(iterations = *iterations, "advancing tool queue");
    if *iterations <= max_tool_iterations {
        return false;
    }
    tracing::warn!(
        iterations = *iterations,
        max_tool_iterations,
        run_id = ?own_run_id,
        "max tool-use iterations exceeded"
    );
    let message = format!("Max tool-use iterations ({max_tool_iterations}) exceeded.");
    match own_run_id {
        None => {
            context.push_assistant(message.clone(), AssistantStatus::Interrupted);
        }
        Some(_) => {
            *iteration_cap_exceeded = Some(message.clone());
        }
    }
    let _ = ui_tx
        .send(UiUpdate::Error {
            message,
            run_id: own_run_id,
        })
        .await;
    *mode = AgentMode::Idle;
    true
}

#[expect(clippy::too_many_arguments)]
pub(super) async fn advance_tool_queue(
    pending_tools: &mut VecDeque<PendingTool>,
    context: &mut Context,
    mode: &mut AgentMode,
    current_cancel: &mut Option<CancellationToken>,
    current_tool_cancel: &mut Option<CancellationToken>,
    iterations: &mut usize,
    max_tool_iterations: usize,
    increment_iterations: bool,
    engine: Arc<dyn Engine>,
    registry: Arc<ToolRegistry>,
    ui_tx: mpsc::Sender<UiUpdate>,
    agent_tx: mpsc::Sender<(u64, AgentEvent)>,
    generation: u64,
    clarify_started_at: &mut Option<std::time::Instant>,
    pending_steers: &mut VecDeque<String>,
    subagent_manager: &mut Option<crate::subagent_manager::SubagentManager>,
    spawn_content_sections: &mut HashMap<Uuid, Vec<crate::agent::tools::Section>>,
    own_run_id: Option<Uuid>,
    pending_subagent_finish_reprompt: &mut bool,
    iteration_cap_exceeded: &mut Option<String>,
    session_id: Option<Uuid>,
    mut compaction: Option<&mut compaction::CompactionRuntime>,
) {
    // `spawn_subagent` and auto-denied tools resolve synchronously within
    // this same loop (unlike a normally-executing auto-approved tool, which
    // `return`s here and waits for a later `ToolDone`/`ToolError` event to
    // re-invoke this function with `increment_iterations: true`). Without
    // this, a round made up entirely of such synchronously-resolved tools
    // could reach the queue-empty re-prompt below still carrying whatever
    // `increment_iterations` this call was originally invoked with (`false`,
    // since the `ToolCall` handler always calls this function that way) —
    // silently completing a full tool-use round without ever counting
    // against the loop's `max_tool_iterations`. Shadow the parameter as mutable so
    // resolving any such tool below can flip it to `true` for the rest of
    // this call.
    let mut increment_iterations = increment_iterations;
    loop {
        match pending_tools.front() {
            None => {
                // Queue empty: optionally increment and re-prompt.
                if increment_iterations
                    && bump_and_check_iteration_cap(
                        iterations,
                        max_tool_iterations,
                        context,
                        mode,
                        &ui_tx,
                        own_run_id,
                        iteration_cap_exceeded,
                    )
                    .await
                {
                    return;
                }
                // All tool results for this round are now in context — this is
                // the earliest safe point to inject queued steers (pending user
                // messages typed while tools were running), so they reach the
                // model on the very next request rather than waiting for a full
                // tool-free turn to complete via StreamDone.
                if inject_pending_steers(pending_steers, context) {
                    let _ = ui_tx
                        .send(UiUpdate::SteerApplied { run_id: own_run_id })
                        .await;
                }
                if let Some(compaction) = compaction.as_deref_mut() {
                    sweep_compaction_and_report(
                        compaction,
                        context,
                        engine.as_ref(),
                        &registry.schemas(),
                        generation,
                        own_run_id,
                        &ui_tx,
                    )
                    .await;
                }
                // This re-prompt's `context` already carries any subagent result
                // that arrived (and was deferred) while the root was mid-turn
                // — clear the flag so a later, unrelated `StreamDone` doesn't
                // force a redundant re-prompt for a result already handled.
                *pending_subagent_finish_reprompt = false;
                *mode = AgentMode::Streaming;
                *current_cancel = Some(start_request(
                    context,
                    compaction,
                    engine,
                    registry.schemas(),
                    ui_tx,
                    agent_tx,
                    generation,
                    own_run_id,
                    session_id,
                ));
                return;
            }
            Some(front) => {
                let result_allowance = tool_output_token_limit(
                    front.call.name(),
                    context,
                    &registry.schemas(),
                    engine.effective_context_length(),
                    &queued_tool_names(pending_tools, 1),
                );
                if let Some(allowance) = result_allowance {
                    if allowance < MINIMUM_USEFUL_RESULT_TOKENS {
                        if compaction.as_deref_mut().is_some_and(|compaction_state| {
                            compaction_state.defer_request(
                                context,
                                engine.as_ref(),
                                &registry.schemas(),
                            )
                        }) {
                            let _ = ui_tx.send(UiUpdate::Notice {
                                text: "Paused not-yet-started tools for the existing context summary; running tools are not cancelled.".to_string(),
                                run_id: own_run_id,
                            }).await;
                            return;
                        }
                        let skipped = pending_tools.pop_front().expect("checked front");
                        let name = skipped.call.name().to_string();
                        resolve_skipped_tool_call(
                            context,
                            &ui_tx,
                            own_run_id,
                            skipped.call.id(),
                            name.clone(),
                            skipped.call.title_arguments(),
                            skipped.call.pending_content_sections(),
                            context_budget_skip_reason(&name),
                        )
                        .await;
                        // Skipped rounds still consume the iteration allowance.
                        increment_iterations = true;
                        continue;
                    }
                }
                let approval = {
                    let _span = tracing::debug_span!(
                        "needs_approval",
                        tool = %front.call.name(),
                        tool_call_id = %front.call.id(),
                    )
                    .entered();
                    // Validate arguments against the tool's input schema first.
                    // Structurally invalid calls are denied immediately without an approval
                    // prompt, for all tools uniformly.
                    let validation = registry
                        .get(front.call.name())
                        .map(|tool| tool.validate_args(front.call.arguments()));
                    match validation {
                        Some(Err(reason)) => crate::sandbox::ApprovalCheck::AutoDenied { reason },
                        _ => registry
                            .get(front.call.name())
                            .map(|tool| tool.needs_approval(front.call.arguments()))
                            .unwrap_or(crate::sandbox::ApprovalCheck::UserApprovalRequired),
                    }
                };
                match approval {
                    crate::sandbox::ApprovalCheck::AutoApproved
                        if front.call.name() == tools::spawn_subagent::SpawnSubagentTool::NAME =>
                    {
                        let pending = pending_tools.pop_front().expect("checked front");
                        resolve_spawn_subagent(
                            pending,
                            subagent_manager,
                            context,
                            &ui_tx,
                            spawn_content_sections,
                        )
                        .await;
                        // Resolves synchronously (unlike a normally-executing
                        // tool); continue the loop to process the next
                        // pending tool or re-prompt. This still counts as a
                        // completed tool-use round against the iteration cap.
                        increment_iterations = true;
                    }
                    crate::sandbox::ApprovalCheck::AutoApproved => {
                        let pending = pending_tools.pop_front().expect("checked front");
                        let name = pending.call.name().to_string();
                        let call_id = pending.call.id();
                        let arguments = pending.call.arguments().clone();
                        let content_sections = pending.call.pending_content_sections();
                        let title_arguments = pending.call.title_arguments();
                        tracing::info!(tool = %name, tool_call_id = %call_id, "tool auto-approved");
                        let max_output_tokens = result_allowance;
                        *current_tool_cancel = Some(PendingTool::execute(
                            pending,
                            agent_tx.clone(),
                            generation,
                            max_output_tokens,
                        ));
                        let _ = ui_tx
                            .send(UiUpdate::ToolStart {
                                name,
                                id: call_id,
                                arguments,
                                content_sections,
                                title_arguments,
                                run_id: own_run_id,
                            })
                            .await;
                        return; // wait for ToolDone/ToolError
                    }
                    crate::sandbox::ApprovalCheck::AutoDenied { reason } => {
                        let pending = pending_tools.pop_front().expect("checked front");
                        let name = pending.call.name().to_string();
                        let call_id = pending.call.id();
                        let title_arguments = pending.call.title_arguments();
                        let content_sections = pending.call.pending_content_sections();
                        tracing::info!(tool = %name, tool_call_id = %call_id, %reason, "tool auto-denied");
                        context.push_tool_error(
                            call_id,
                            name.clone(),
                            reason.clone(),
                            None,
                            std::time::Duration::ZERO,
                            content_sections.clone(),
                        );
                        let _ = ui_tx
                            .send(UiUpdate::ToolAutoDenied {
                                id: call_id,
                                name,
                                title_arguments,
                                content_sections,
                                reason,
                                run_id: own_run_id,
                            })
                            .await;
                        // Continue loop to handle next tool in queue. This
                        // still counts as a completed tool-use round against
                        // the iteration cap (see comment on
                        // `increment_iterations` above).
                        increment_iterations = true;
                    }
                    crate::sandbox::ApprovalCheck::UserApprovalRequired
                        if front.call.name() == tools::clarify::ClarifyTool::NAME =>
                    {
                        let args = front.call.arguments();
                        let question = args
                            .get("question")
                            .and_then(|value| value.as_str())
                            .unwrap_or("")
                            .to_string();
                        let context = args
                            .get("context")
                            .and_then(|value| value.as_str())
                            .map(str::to_string);
                        let choices =
                            args.get("choices")
                                .and_then(|value| value.as_array())
                                .map(|values| {
                                    values
                                        .iter()
                                        .filter_map(|value| value.as_str().map(str::to_string))
                                        .collect()
                                });
                        let call_id = front.call.id();
                        *mode = AgentMode::ClarifyPending;
                        *clarify_started_at = Some(std::time::Instant::now());
                        let _ = ui_tx
                            .send(UiUpdate::ClarifyPending {
                                id: call_id,
                                question,
                                context,
                                choices,
                                run_id: own_run_id,
                            })
                            .await;
                        return;
                    }
                    crate::sandbox::ApprovalCheck::UserApprovalRequired => {
                        log_tool_approval_request(front.call.as_ref());
                        *mode = AgentMode::ToolPending;
                        let _ = ui_tx
                            .send(UiUpdate::ToolPending {
                                name: front.call.name().to_string(),
                                id: front.call.id(),
                                arguments: front.call.arguments().clone(),
                                content_sections: front.call.pending_content_sections(),
                                title_arguments: front.call.title_arguments(),
                                run_id: own_run_id,
                            })
                            .await;
                        return;
                    }
                }
            }
        }
    }
}

/// Drains queued steers into context as user messages; returns true if any
/// were injected. Injecting is the only way a steer reaches the LLM — it
/// never cancels a live stream.
pub(super) fn inject_pending_steers(
    pending_steers: &mut VecDeque<String>,
    context: &mut Context,
) -> bool {
    if pending_steers.is_empty() {
        return false;
    }
    while let Some(message) = pending_steers.pop_front() {
        context.push_user(message);
    }
    true
}

pub(super) async fn admit_tool_batch(
    calls: Vec<AssembledToolCall>,
    registry: &ToolRegistry,
    pending_tools: &mut VecDeque<PendingTool>,
    context: &mut Context,
    ui_tx: &mpsc::Sender<UiUpdate>,
    own_run_id: Option<Uuid>,
    batch_cap: usize,
) -> bool {
    let mut requests = Vec::with_capacity(calls.len());
    let mut unknown_tools = Vec::new();
    let mut skipped_calls = Vec::new();
    for (position, assembled) in calls.into_iter().enumerate() {
        let pending_call =
            registry.create_call(&assembled.name, assembled.id, assembled.arguments.clone());
        let persisted_title_arguments = pending_call
            .as_ref()
            .map_or_else(Vec::new, |call| call.title_arguments());
        requests.push(ToolCallRequest {
            id: assembled.id,
            name: assembled.name.clone(),
            arguments: assembled.arguments.clone(),
            persisted_title_arguments,
        });
        match pending_call {
            Some(call) if position < batch_cap => {
                pending_tools.push_back(PendingTool { call });
            }
            Some(call) => {
                skipped_calls.push((
                    assembled.id,
                    assembled.name.clone(),
                    call.title_arguments(),
                    call.pending_content_sections(),
                ));
            }
            None if position >= batch_cap => {
                skipped_calls.push((assembled.id, assembled.name.clone(), Vec::new(), Vec::new()));
            }
            None => {
                let error = format!("Unknown tool '{}'", assembled.name);
                unknown_tools.push((assembled.id, assembled.name, error));
            }
        }
    }
    context.record_batch_admission(requests.len(), unknown_tools.len());
    context.push_assistant_tool_call(String::new(), AssistantStatus::Complete, requests);
    let unknown_only_round =
        (!unknown_tools.is_empty() || !skipped_calls.is_empty()) && pending_tools.is_empty();
    for (id, name, title_arguments, content_sections) in skipped_calls {
        let reason = batch_limit_skip_reason(&name, batch_cap);
        resolve_skipped_tool_call(
            context,
            ui_tx,
            own_run_id,
            id,
            name,
            title_arguments,
            content_sections,
            reason,
        )
        .await;
    }
    // Registry failures are synchronous results, not
    // completions of the tool currently executing.
    for (id, name, error) in unknown_tools {
        tracing::error!(tool = %name, tool_call_id = %id, error = %error, "tool failed");
        context.push_tool_error(
            id,
            name,
            error.clone(),
            None,
            std::time::Duration::ZERO,
            vec![],
        );
        let _ = ui_tx
            .send(UiUpdate::ToolError {
                id,
                error,
                duration: std::time::Duration::ZERO,
                content_sections: vec![],
                run_id: own_run_id,
            })
            .await;
    }

    unknown_only_round
}
