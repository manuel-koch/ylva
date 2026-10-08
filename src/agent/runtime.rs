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
mod commands;
pub(super) mod compaction_updates;
mod events;
mod subagents;
use self::compaction_updates::{cancelled_or_pending, recv_finish, recv_subagent_ui};
use self::compaction_updates::{report_compaction_failure, sweep_compaction_and_report};

struct RunState {
    engine: Arc<dyn Engine>,
    registry: Arc<ToolRegistry>,
    ui_tx: mpsc::Sender<UiUpdate>,
    agent_tx: mpsc::Sender<(u64, AgentEvent)>,
    parent_cancel: Option<CancellationToken>,
    system_prompt: Option<String>,
    own_run_id: Option<Uuid>,
    child_agent_manager: Option<crate::child_agent_manager::ChildAgentManager>,
    max_tool_iterations: usize,
    batch_cap: usize,
    agent_type: Option<String>,
    session_id: Option<Uuid>,
    current_store: Arc<Mutex<Option<Store>>>,
    store_for_commands: Option<Store>,
    spawn_content_sections: HashMap<Uuid, Vec<tools::Section>>,
    is_child: bool,
    context: Context,
    compaction: compaction::CompactionRuntime,
    startup_checks_finished: bool,
    request_waiting_for_startup_checks: bool,
    model_change_capacity_check: Option<futures::future::BoxFuture<'static, Option<String>>>,
    mode: AgentMode,
    current_cancel: Option<CancellationToken>,
    current_tool_cancel: Option<CancellationToken>,
    pending_steers: VecDeque<String>,
    pending_tools: VecDeque<PendingTool>,
    iterations: usize,
    clarify_started_at: Option<std::time::Instant>,
    generation: u64,
    cancelled_run_ids: std::collections::HashSet<Uuid>,
    pending_child_finish_reprompt: bool,
    iteration_cap_exceeded: Option<String>,
    stream_error: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum LoopControl {
    PollAgain,
    CheckCompletion,
    Finished,
}

impl RunState {
    fn start_request(&mut self) {
        self.current_cancel = Some(start_request(
            &mut self.context,
            Some(&mut self.compaction),
            self.engine.clone(),
            self.registry.schemas(),
            self.ui_tx.clone(),
            self.agent_tx.clone(),
            self.generation,
            self.own_run_id,
            self.session_id,
        ));
    }

    fn resume_after_compaction(&mut self) {
        self.mode = AgentMode::Streaming;
        self.start_request();
    }

    async fn advance_tool_queue(&mut self, count_iteration: bool) {
        advance_tool_queue(
            &mut self.pending_tools,
            &mut self.context,
            &mut self.mode,
            &mut self.current_cancel,
            &mut self.current_tool_cancel,
            &mut self.iterations,
            self.max_tool_iterations,
            count_iteration,
            self.engine.clone(),
            self.registry.clone(),
            self.ui_tx.clone(),
            self.agent_tx.clone(),
            self.generation,
            &mut self.clarify_started_at,
            &mut self.pending_steers,
            &mut self.child_agent_manager,
            &mut self.spawn_content_sections,
            self.own_run_id,
            &mut self.pending_child_finish_reprompt,
            &mut self.iteration_cap_exceeded,
            self.session_id,
            Some(&mut self.compaction),
        )
        .await;
    }
}

impl AgentLoop {
    pub fn run(&mut self) -> tokio::task::JoinHandle<anyhow::Result<Context>> {
        let engine = self.engine.clone();
        let registry = self.registry.clone();
        let ui_tx = self.ui_tx.clone();
        let agent_tx = self.agent_tx.clone();
        let mut cmd_rx = self
            .cmd_rx
            .take()
            .expect("AgentLoop run() must not be called more than once");
        let mut agent_rx = self
            .agent_rx
            .take()
            .expect("AgentLoop run() must not be called more than once");
        let parent_cancel = self.parent_cancel.take();
        let briefing = self.briefing.take();
        let system_prompt = self.system_prompt.clone();
        let own_run_id = self.own_run_id.take();
        let child_agent_manager = self.child_agent_manager.take();
        let mut finish_rx = self.finish_rx.take();
        let mut subagent_ui_rx = self.subagent_ui_rx.take();
        let max_tool_iterations = self.max_tool_iterations;
        #[cfg(test)]
        let batch_cap = self.test_batch_cap;
        #[cfg(not(test))]
        let batch_cap = MAX_EXECUTED_TOOL_CALLS_PER_BATCH;
        let compaction_engine = self.compaction_engine.clone();
        let compaction_timeout = self.compaction_timeout;
        let compaction_summary_prompt = std::mem::take(&mut self.compaction_summary_prompt);
        let startup_compaction_checks = std::mem::take(&mut self.startup_compaction_checks);
        let agent_type = self.agent_type.clone();
        let store = self.store.clone();
        let session_id = self.session_id;
        let current_store = Arc::clone(&self.current_store);
        // Retained past `Context::set_store` so `AgentCommand::ClearContext`
        // can roll the session over to a fresh file on the same store root.
        let store_for_commands = store.clone();
        // Stashes each in-flight `spawn_subagent` call's task/briefing content
        // sections (shown while pending/running) keyed by call id, so they
        // can be carried forward into the eventual real `ToolDone` — sent
        // once the child actually finishes or is cancelled — alongside its
        // result. Entries are removed as soon as that terminal `ToolDone` is
        // sent; see `resolve_spawn_subagent`, the `finish_rx` arm below, and
        // the `AgentCommand::Stop` handler's cancellation cleanup.
        let spawn_content_sections: HashMap<Uuid, Vec<crate::agent::tools::Section>> =
            HashMap::new();
        // Only a `Role::Child` loop has `parent_cancel` set; such a loop must
        // return as soon as it goes idle instead of waiting forever for
        // commands nobody will send (see the end of the loop body below).
        let is_child = parent_cancel.is_some();

        tokio::spawn(async move {
            let mut context = match system_prompt.as_deref() {
                Some(system_prompt) => Context::with_system_prompt(system_prompt),
                None => Context::new(),
            };
            // Pick the common local estimate before hydration, so counts
            // cached while replaying use the same baseline.
            context.set_token_estimator(crate::llm::TokenEstimator::Tiktoken);
            if let Some(store) = store {
                context.set_store(store);
            }
            let (mut compaction, mut compaction_rx) = compaction::CompactionRuntime::new(
                compaction_engine.clone(),
                compaction_summary_prompt.clone(),
                compaction_timeout,
            );
            compaction.set_activity_sender(ui_tx.clone(), own_run_id);
            let root_capacity_warning = compaction.capacity_warning(engine.clone());
            let startup_ui_tx = ui_tx.clone();
            let startup_agent_label = agent_type
                .as_ref()
                .map(|agent_type| format!("Agent type '{agent_type}'"))
                .unwrap_or_else(|| "Root agent".to_string());
            let startup_checks = async move {
                let mut warnings = Vec::new();
                if let Some(text) = root_capacity_warning.await {
                    warnings.push((startup_agent_label, text));
                }
                for check in startup_compaction_checks {
                    if let Some(text) = check.warning(&compaction_summary_prompt).await {
                        warnings.push((format!("Agent type '{}'", check.agent_type), text));
                    }
                }
                for text in compaction::group_capacity_warnings(warnings) {
                    compaction::report_capacity_warning(&startup_ui_tx, text, own_run_id).await;
                }
            };
            tokio::pin!(startup_checks);
            let startup_checks_finished = false;
            let request_waiting_for_startup_checks = false;
            let model_change_capacity_check: Option<
                futures::future::BoxFuture<'static, Option<String>>,
            > = None;
            if let Some(briefing) = briefing {
                context.push_system(briefing);
            }
            if is_child {
                // Give the child a chance to budget its own tool calls: it
                // has no visibility into `advance_tool_queue`'s guard (it
                // cannot tell "was cut off" from "chose to stop"), so a smart
                // model can only avoid the cap by knowing about it up front
                // and wrapping up (calling `agent_finish`) well before it
                // hits.
                context.push_system(format!(
                    "You have a budget of at most {max_tool_iterations} tool-use \
                     round(s) to complete this task. Work efficiently — batch related \
                     tool calls where possible — and call `agent_finish` with your \
                     result as soon as you have an answer, well before you run out of \
                     rounds. If you exhaust this budget without calling `agent_finish`, \
                     your work will be reported as failed and none of your findings will \
                     reach the caller."
                ));
            }
            let mode = AgentMode::Idle;
            let current_cancel: Option<CancellationToken> = None;
            let current_tool_cancel: Option<CancellationToken> = None;
            let pending_steers: VecDeque<String> = VecDeque::new();
            let pending_tools: VecDeque<PendingTool> = VecDeque::new();
            let iterations = 0usize;
            let clarify_started_at: Option<std::time::Instant> = None;
            // Bumped by `AgentCommand::ClearContext` so any `AgentEvent`
            // produced by a stream/tool started *before* the clear (and
            // still in flight when it happened — cancellation is
            // cooperative, not synchronous) is recognised as stale and
            // dropped when it eventually arrives, instead of being applied
            // to the unrelated `fresh_context` the clear installed. Every
            // `start_request`/`PendingTool::execute` call stamps the
            // `generation` active at spawn time onto its events; the
            // `agent_rx` arm below only processes an event whose stamped
            // generation still matches this counter.
            let generation: u64 = 0;
            // Populated by `AgentCommand::Stop`/`AgentCommand::ClearContext`
            // right after `child_agent_manager.cancel_all()`: the `run_id`s
            // of children cancelled but not yet confirmed terminal. A
            // cancelled child's task may still be mid-flight and send a real
            // `FinishResult` after cancellation was requested; `finish_rx`'s
            // arm consults this set to drop such stale results instead of
            // pushing their content into a `Context` (possibly a brand-new
            // one, post-`ClearContext`) they no longer belong to.
            let cancelled_run_ids: std::collections::HashSet<Uuid> =
                std::collections::HashSet::new();
            // Set by the `finish_rx` arm when the final outstanding child's
            // result lands while the root is mid-stream (`mode != Idle`): the
            // result is already in `context` (see the `finish_rx` arm's
            // comment), but the root can't react to it until its *own*
            // in-flight turn completes.
            // `StreamDone` below is that turn's completion point, so it must
            // check this flag and force a re-prompt — otherwise the root goes
            // Idle believing its just-streamed text is the final answer, the
            // driver (which tracks the same child via `pending_spawn_call_ids`)
            // sees zero pending children and ends the conversation, and the
            // child's result silently never reaches the model.
            let pending_child_finish_reprompt = false;
            // Set (only for a child loop) when `advance_tool_queue`'s
            // max-tool-use-iterations guard trips — see that function's doc
            // comment. Checked below at the `is_child && mode == Idle` tail
            // to report a clear `Err` (→ `status: "failure"`) instead of the
            // generic "unstructured" `Ok(context)` fallback.
            let iteration_cap_exceeded: Option<String> = None;
            // A child has no caller that can recover from a provider/stream
            // failure after the current request ends. Preserve that
            // authoritative error so the child returns `Err` instead of
            // being misclassified as an unstructured successful answer.
            let stream_error: Option<String> = None;

            let mut state = RunState {
                engine,
                registry,
                ui_tx,
                agent_tx,
                parent_cancel,
                system_prompt,
                own_run_id,
                child_agent_manager,
                max_tool_iterations,
                batch_cap,
                agent_type,
                session_id,
                current_store,
                store_for_commands,
                spawn_content_sections,
                is_child,
                context,
                compaction,
                startup_checks_finished,
                request_waiting_for_startup_checks,
                model_change_capacity_check,
                mode,
                current_cancel,
                current_tool_cancel,
                pending_steers,
                pending_tools,
                iterations,
                clarify_started_at,
                generation,
                cancelled_run_ids,
                pending_child_finish_reprompt,
                iteration_cap_exceeded,
                stream_error,
            };

            loop {
                tokio::select! {
                biased;
                _ = &mut startup_checks, if !state.startup_checks_finished => {
                                            state.startup_checks_finished = true;
                                            if std::mem::take(&mut state.request_waiting_for_startup_checks) {
                                                state.start_request();
                                            }
                                            // Startup completion is not a completed child turn.
                                            continue;
                                        }
                warning = async {
                                            match state.model_change_capacity_check.as_mut() {
                                                Some(check) => check.await,
                                                None => std::future::pending().await,
                                            }
                                        }, if state.model_change_capacity_check.is_some() => {
                                            state.model_change_capacity_check = None;
                                            if let Some(text) = warning {
                                                compaction::report_capacity_warning(&state.ui_tx, text, state.own_run_id).await;
                                            }
                                            continue;
                                        }
                Some((event_generation, event)) = agent_rx.recv() => {
                match state.handle_agent_event(event_generation, event).await {
                LoopControl::PollAgain => continue,
                LoopControl::Finished => return Ok(state.context),
                LoopControl::CheckCompletion => {}
                }
                }
                Some(finish_result) = recv_finish(&mut finish_rx) => {
                match state.handle_subagent_finish(finish_result).await {
                LoopControl::PollAgain => continue,
                LoopControl::Finished => return Ok(state.context),
                LoopControl::CheckCompletion => {}
                }
                }
                Some(update) = recv_subagent_ui(&mut subagent_ui_rx) => { state.forward_subagent_update(update).await; }
                _ = cancelled_or_pending(&state.parent_cancel) => {
                                            // The parent cancelled this child (e.g. it stopped
                                            // or was torn down). Abort whatever is in flight and
                                            // return immediately — nobody will drive this loop
                                            // with further commands.
                                            tracing::info!("child agent cancelled by parent");
                                            if let Some(cancel_token) = state.current_cancel.take() {
                                                cancel_token.cancel();
                                            }
                                            if let Some(tool_cancel) = state.current_tool_cancel.take() {
                                                tool_cancel.cancel();
                                            }
                                            state.pending_tools.clear();
                                            state.pending_steers.clear();
                                            return Ok(state.context);
                                        }
                outcome = compaction_rx.recv() => {
                                            // A background summary finished (or failed). The
                                            // loop itself applies it — never the summarizer —
                                            // and only at a safe boundary: idle with an empty
                                            // serial tool queue. Otherwise it stays pending
                                            // until the next completed turn.
                                            if let Some(outcome) = outcome {
                                                let summary_failure = match &outcome {
                                                    compaction::CompactionOutcome::SummaryFailed { reason, .. } => {
                                                        Some(reason.clone())
                                                    }
                                                    compaction::CompactionOutcome::SummaryReady { .. } => None,
                                                };
                                                if !state.compaction.on_outcome(outcome, state.generation) {
                                                    continue;
                                                }
                                                let waiting_continuation = state.compaction.take_waiting_continuation();
                                                if (state.mode == AgentMode::Idle || waiting_continuation) && state.pending_tools.is_empty() {
                                                    let prefix_dropped = sweep_compaction_and_report(
                                                        &mut state.compaction,
                                                        &mut state.context,
                                                        state.engine.as_ref(),
                                                        &state.registry.schemas(),
                                                        state.generation,
                                                        state.own_run_id,
                                                        &state.ui_tx,
                                                    )
                                                    .await;
                                                    if waiting_continuation || (state.is_child && prefix_dropped) {
                                                        state.resume_after_compaction();
                                                    }
                                                } else if waiting_continuation {
                                                    state.advance_tool_queue(false).await;
                                                }
                                                if let Some(reason) = summary_failure {
                                                    report_compaction_failure(&state.ui_tx, state.own_run_id, &reason).await;
                                                }
                                            }
                                        }
                cmd = cmd_rx.recv() => {
                match state.handle_command(cmd).await {
                LoopControl::PollAgain => continue,
                LoopControl::Finished => return Ok(state.context),
                LoopControl::CheckCompletion => {}
                }
                }
                }

                // A `Role::Child` loop has nobody left to drive it once it
                // goes idle (no user, no more steers) — the "unstructured
                // fallback" case (the model stopped calling tools without
                // ever calling `agent_finish`). Returning here lets the caller
                // (the spawn wrapper) harvest the final `Context` and report
                // an "unstructured" outcome.
                //
                // Iteration-cap and stream-error cases are different and
                // reported as hard `Err`s instead: the child either never got
                // a chance to conclude naturally or the provider failed, so
                // there is no "final answer" to salvage. The
                // `drive_child_to_completion` `Ok(Err(error))` arm reports
                // exactly the right shape (`status: "failure"`, explicit
                // `error`, no fabricated `result`).
                if state.is_child && state.mode == AgentMode::Idle {
                    if state.compaction.summary_pending() {
                        continue;
                    }
                    if state.compaction.enabled() && state.context.compaction().is_none() {
                        let prefix_dropped = sweep_compaction_and_report(
                            &mut state.compaction,
                            &mut state.context,
                            state.engine.as_ref(),
                            &state.registry.schemas(),
                            state.generation,
                            state.own_run_id,
                            &state.ui_tx,
                        )
                        .await;
                        if prefix_dropped {
                            state.resume_after_compaction();
                            continue;
                        }
                        if state.compaction.summary_pending() {
                            continue;
                        }
                    }
                    if let Some(message) = state.stream_error.take() {
                        return Err(anyhow::anyhow!(message));
                    }
                    if let Some(message) = state.iteration_cap_exceeded.take() {
                        return Err(anyhow::anyhow!(message));
                    }
                    return Ok(state.context);
                }
            }
        })
    }
}

#[cfg(test)]
mod tests;
