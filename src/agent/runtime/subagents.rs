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

impl RunState {
    pub(super) async fn handle_subagent_finish(
        &mut self,
        finish_result: crate::child_agent_manager::FinishResult,
    ) -> LoopControl {
        if self.session_switch_frozen {
            tracing::debug!("ignoring child completion while session replacement is frozen");
            return LoopControl::PollAgain;
        }
        // The tool-calling wire protocol allows exactly one
        // `role:"tool"` response per `tool_call_id`; that slot
        // was already spent on `spawn_subagent`'s immediate
        // `{"status":"running"/"queued"}` result (see
        // `resolve_spawn_subagent`). So the child's *real*
        // outcome, arriving asynchronously and possibly after
        // further assistant turns, is appended as a brand-new
        // hidden harness notice instead of rewriting/duplicating
        // that earlier tool result — this keeps history
        // append-only (an honest record of what was known
        // when) and avoids the duplicate-`tool_call_id`
        // protocol violation entirely, rather than papering
        // over it by mutating a past message in place. A
        // completion is appended to context immediately, but
        // the root is re-prompted only after all currently
        // outstanding child runs are terminal. This avoids
        // acknowledgement turns for intermediate results
        // while preserving a single continuation point for
        // the aggregate answer.
        let run_id = finish_result.run_id;
        if self.cancelled_run_ids.remove(&run_id) {
            // This child was cancelled by a `Stop`/`ClearContext`
            // that already resolved (or discarded) its
            // tool bubble/context entry synchronously; the
            // manager only *requested* cancellation (a
            // `CancellationToken` is cooperative, not
            // immediate), so the child's task can still race
            // in a real `FinishResult` afterwards. Applying it
            // now would duplicate an already-settled entry or
            // — after `ClearContext` — push content into an
            // unrelated, brand-new `Context`. Drop it.
            tracing::debug!(
                %run_id,
                tool_call_id = %finish_result.call_id,
                "dropping stale child finish result for a cancelled run"
            );
            return LoopControl::PollAgain;
        }
        let call_id = finish_result.call_id;
        let status = finish_result.status.clone();
        let label = self
            .child_agent_manager
            .as_ref()
            .and_then(|manager| manager.label_for(run_id))
            .unwrap_or_default()
            .to_string();
        let outstanding_count = self
            .child_agent_manager
            .as_ref()
            .map(|manager| manager.outstanding_count_after_completion(run_id))
            .unwrap_or(0);
        let all_done = outstanding_count == 0;
        if self.mode == AgentMode::Idle && self.pending_tools.is_empty() {
            // Apply any completed prefix summary or hard
            // fallback before deciding whether this child
            // report itself needs summarization.
            sweep_compaction_and_report(
                &mut self.compaction,
                &mut self.context,
                self.engine.as_ref(),
                &self.registry.schemas(),
                self.generation,
                self.own_run_id,
                &self.ui_tx,
            )
            .await;
        }
        let mut parent_result = finish_result.result.clone();
        let result_token_budget = self.context.available_result_token_budget(
            &self.registry.schemas(),
            self.engine.effective_context_length(),
        );
        let original_parent_result = parent_result.clone();
        if let Some(result) = parent_result.as_deref() {
            let result_estimator = crate::llm::TokenEstimator::Tiktoken;
            if result_estimator.estimate_tokens(result) > result_token_budget {
                parent_result = Some(compaction::recovery::marked_text(
                    result_estimator,
                    result,
                    compaction::recovery::RESULT_OMISSION_MARKER,
                    result_token_budget / 2,
                ));
            }
        }
        // The LLM-facing notice stays structured JSON (the
        // model reads correlation/lifecycle fields as well as
        // `status`/`result`/`error`/`sources`); only the TUI
        // bubble renders result content as Markdown sections.
        let mut completion_event = finish_result.to_completion_json(&label, outstanding_count);
        completion_event["result"] = parent_result
            .clone()
            .map(serde_json::Value::String)
            .unwrap_or(serde_json::Value::Null);
        completion_event["result_summarized"] = serde_json::Value::Bool(false);
        completion_event["result_reduced"] =
            serde_json::Value::Bool(original_parent_result != parent_result);
        let result_text = serde_json::to_string(&completion_event).unwrap_or_default();
        let mut content_sections = self
            .spawn_content_sections
            .remove(&call_id)
            .unwrap_or_default();
        content_sections.extend(finish_result.content_sections());
        tracing::info!(%run_id, tool_call_id = %call_id, %status, "child agent finished");
        let next_step_note = if !all_done {
            format!(
                                    "{outstanding_count} other subagent run(s) remain outstanding. \
                                     This is an intermediate completion; do not produce an \
                                     acknowledgement or a user-facing progress update solely for \
                                     this event. The harness will provide one continuation after \
                                     all outstanding child results are available. This is expected, \
                                     normal operation — do not reason about scheduler/concurrency \
                                     internals or speculate on timing. Continue only genuinely \
                                     independent work. If none remains, end this response \
                                     immediately, without waiting narration. Do not keep thinking \
                                     while waiting for results. Ending this response does not \
                                     cancel the subagents or complete the user's task."
                                )
        } else {
            "All spawned subagent runs are now terminal. Combine this result with \
                                 earlier completion events in the conversation and produce the \
                                 answer for the user."
                .to_string()
        };
        let projected_notice = format!(
            "Subagent completion event; the structured JSON is harness metadata, \
                                 not a user-facing message. Do not assume tool results or subagent \
                                 responses are visible to the user. When all_done is true, use the \
                                 relevant findings, sources, and limitations from these results to \
                                 produce a self-contained answer to the user's request. Preserve \
                                 the user's requested format, including tables. Present the actual \
                                 findings, not just a completion acknowledgement or a reference to \
                                 results 'above'. Report failures or missing findings explicitly; \
                                 do not invent details omitted from a reduced result. Do not quote \
                                 the harness JSON verbatim:\n{result_text}\n\
                                 {next_step_note}",
        );
        let mut canonical_event = finish_result.to_completion_json(&label, outstanding_count);
        canonical_event["result_summarized"] = serde_json::Value::Bool(false);
        canonical_event["result_reduced"] = serde_json::Value::Bool(false);
        self.context.push_async_harness_notice(
            projected_notice.replace(&result_text, &canonical_event.to_string()),
        );
        if original_parent_result != parent_result {
            self.context.project_last_harness_notice(projected_notice);
        }
        self.context.limit_last_model_message(result_token_budget);
        // Computed before `on_child_finished` clears the running slot.
        let duration = self
            .child_agent_manager
            .as_ref()
            .and_then(|manager| manager.running_duration(run_id))
            .unwrap_or_default();
        // Emitted before the `spawn_subagent` bubble's own
        // `ToolDone` so the driver can auto-return from the
        // child conversation view (if it auto-switched into
        // it) ahead of the parent bubble settling.
        let _ = self
            .ui_tx
            .send(UiUpdate::ChildAgentDone {
                run_id,
                status: status.clone(),
                result: finish_result.result.clone(),
            })
            .await;
        let _ = self
            .ui_tx
            .send(UiUpdate::ToolDone {
                id: call_id,
                name: tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
                result_section: crate::tui_reducer_state::ResultSection::plain(String::new()),
                summary: vec![("status".to_string(), status)],
                duration,
                content_sections,
                run_id: None,
            })
            .await;
        if let Some(manager) = self.child_agent_manager.as_mut() {
            manager.record_child_result(&finish_result);
            manager.on_child_finished(run_id);
        }
        tracing::debug!(
            tool_call_id = %call_id,
            mode = ?self.mode,
            "finish_rx: deciding whether to reprompt root after child result"
        );
        if all_done && self.mode == AgentMode::Idle {
            // Same iteration budget as any other re-prompt
            // (`advance_tool_queue`'s queue-empty branch) —
            // without this guard a model that just keeps
            // calling `spawn_subagent` again on every finish
            // notice, instead of ever answering, loops here
            // forever: this branch used to call
            // `start_request` unconditionally, bypassing the
            // cap entirely (see `bump_and_check_iteration_cap`'s
            // doc comment).
            if bump_and_check_iteration_cap(
                &mut self.iterations,
                self.max_tool_iterations,
                &mut self.context,
                &mut self.mode,
                &self.ui_tx,
                self.own_run_id,
                &mut self.iteration_cap_exceeded,
            )
            .await
            {
                tracing::info!(
                    tool_call_id = %call_id,
                    "finish_rx: root was idle but iteration cap already exceeded; not reprompting"
                );
            } else {
                tracing::info!(tool_call_id = %call_id, "finish_rx: root was idle, forcing reprompt");
                self.mode = AgentMode::Streaming;
                self.start_request();
            }
        } else if all_done {
            self.pending_child_finish_reprompt = true;
            tracing::debug!(
                tool_call_id = %call_id,
                mode = ?self.mode,
                "finish_rx: root not idle, deferring to its own queue-drain re-prompt"
            );
        } else {
            tracing::debug!(
                tool_call_id = %call_id,
                outstanding_count,
                mode = ?self.mode,
                "finish_rx: intermediate child result recorded without reprompt"
            );
        }
        LoopControl::CheckCompletion
    }
    pub(super) async fn forward_subagent_update(&mut self, update: UiUpdate) {
        // Phase 6.10 drill-in conversation model: the
        // child's own text/thinking tokens must never leak
        // into the parent's own `partial_assistant_text`
        // (they render in the dedicated child transcript
        // instead), and the child's own stream lifecycle
        // (`StreamStarted`/`StreamDone`/`StreamCancelled`)
        // must never flip the parent's own `TuiMode` —
        // only `ChildAgentStarted`/`ChildAgentDone` (see
        // `ChildAgentManager::start` and this loop's
        // `finish_rx` arm) mark the child conversation's
        // lifecycle boundaries for the UI. Everything else
        // (tool approvals/progress/errors, already stamped
        // with the child's own `run_id`) is forwarded as-is.
        match update {
            // Child text and thinking arrive already attributed to their own
            // `run_id` (see `attribute_child_update`), so no running-child
            // lookup is needed and concurrent children stay separated.
            UiUpdate::AppendText(_) | UiUpdate::AppendThinking(_) => {
                tracing::warn!("dropping child text that was not attributed to a run");
            }
            UiUpdate::StreamStarted
            | UiUpdate::StreamDone
            | UiUpdate::StreamCancelled
            | UiUpdate::AgentIdle => {
                // Intentionally swallowed — see comment above.
            }
            update @ UiUpdate::ChildContextUsage { .. } => {
                // Child readouts are already scoped to their
                // run; forward them so the TUI can retain each
                // child independently of the parent view.
                let _ = self.ui_tx.send(update).await;
            }
            other => {
                let _ = self.ui_tx.send(other).await;
            }
        }
    }
}
