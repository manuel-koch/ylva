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
    pub(super) async fn handle_agent_event(
        &mut self,
        event_generation: u64,
        event: AgentEvent,
    ) -> LoopControl {
        if self.session_switch_frozen {
            tracing::debug!("ignoring agent event while session replacement is frozen");
            return LoopControl::PollAgain;
        }
        if event_generation != self.generation {
            tracing::debug!(
                event_generation,
                generation = self.generation,
                ?event,
                "dropping stale agent event from a superseded generation \
                                     (likely produced before an AgentCommand::ClearContext)"
            );
            return LoopControl::PollAgain;
        }
        match event {
            AgentEvent::StreamDone { partial } => {
                self.current_cancel = None;
                self.context
                    .push_assistant(partial, AssistantStatus::Complete);
                // Signal the driver only after the completed
                // assistant message has entered Context and
                // the session store. This ordering lets
                // out-of-band consumers safely inspect the
                // committed turn.
                let _ = self.ui_tx.send(UiUpdate::StreamDone).await;
                let steers_injected =
                    inject_pending_steers(&mut self.pending_steers, &mut self.context);
                if steers_injected {
                    self.pending_child_finish_reprompt = false;
                    let _ = self
                        .ui_tx
                        .send(UiUpdate::SteerApplied {
                            run_id: self.own_run_id,
                        })
                        .await;
                    self.mode = AgentMode::Streaming;
                    self.start_request();
                } else if self.pending_child_finish_reprompt {
                    self.pending_child_finish_reprompt = false;
                    // Same iteration budget as any other
                    // re-prompt — without this guard a model
                    // whose replies never contain a tool call
                    // (so this branch, not `advance_tool_queue`'s
                    // queue-empty branch, is the one that
                    // clears a finish notice deferred while it
                    // was still streaming) could reprompt
                    // forever, bypassing the cap entirely; see
                    // `bump_and_check_iteration_cap`'s doc
                    // comment.
                    if !bump_and_check_iteration_cap(
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
                        self.mode = AgentMode::Streaming;
                        self.start_request();
                    }
                } else {
                    self.mode = AgentMode::Idle;
                }
                if self.mode == AgentMode::Idle && self.pending_tools.is_empty() {
                    let prefix_dropped = sweep_compaction_and_report(
                        &mut self.compaction,
                        &mut self.context,
                        self.engine.as_ref(),
                        &self.registry.schemas(),
                        self.generation,
                        self.own_run_id,
                        &self.ui_tx,
                    )
                    .await;
                    if self.is_child && prefix_dropped {
                        self.resume_after_compaction();
                    }
                }
            }
            AgentEvent::StreamCancelled { partial } => {
                // Only commit the partial when this was a genuine
                // interrupt; ignore stale cancellation from a replaced
                // stream (current_cancel already Some again).
                if self.current_cancel.is_none() {
                    self.context
                        .push_assistant(partial, AssistantStatus::Interrupted);
                    self.mode = AgentMode::Idle;
                }
            }
            AgentEvent::PreflightNotice {
                text,
                acknowledgement,
            } => {
                let _ = self
                    .ui_tx
                    .send(UiUpdate::Notice {
                        text,
                        run_id: self.own_run_id,
                    })
                    .await;
                if let Some(acknowledgement) = acknowledgement
                    .lock()
                    .expect("preflight acknowledgement lock")
                    .take()
                {
                    let _ = acknowledgement.send(());
                }
            }
            AgentEvent::StreamError(error) => {
                self.pending_steers.clear();
                self.current_cancel = None;
                self.mode = AgentMode::Idle;
                if self.is_child {
                    self.stream_error = Some(error.clone());
                }
                tracing::error!(message = %error, "agent stream error");
                let _ = self
                    .ui_tx
                    .send(UiUpdate::Error {
                        message: error,
                        run_id: self.own_run_id,
                    })
                    .await;
            }
            AgentEvent::ContextWindowRejected(error) => {
                tracing::warn!(
                    estimated_tokens = self.context.estimated_model_tokens(&self.registry.schemas(), self.engine.as_ref()),
                    context_length = self.engine.effective_context_length(),
                    reason = %error,
                    "provider rejected context; retrying once with request-local extra margin"
                );
                self.context.retry_after_context_rejection();
                self.start_request();
            }
            AgentEvent::ContextWindowChanged => {
                tracing::debug!(
                    context_length = self.engine.effective_context_length(),
                    "context window changed during preflight; re-evaluating the request budget"
                );
                self.start_request();
            }
            AgentEvent::ToolCallLimitExceeded { limit } => {
                self.current_cancel = None;
                self.pending_steers.clear();
                let notice = format!(
                                        "The previous assistant response exceeded the limit of {limit} tool calls. None of those calls were executed. Continue the task using no more than {limit} tool calls in one response."
                                    );
                tracing::warn!(limit, "assistant response exceeded tool-call limit");
                self.context.push_async_harness_notice(notice);
                if !bump_and_check_iteration_cap(
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
                    self.mode = AgentMode::Streaming;
                    self.start_request();
                }
            }
            AgentEvent::ToolCall { calls } => {
                let unknown_only_round = tool_queue::admit_tool_batch(
                    calls,
                    &self.registry,
                    &mut self.pending_tools,
                    &mut self.context,
                    &self.ui_tx,
                    self.own_run_id,
                    self.batch_cap,
                )
                .await;
                self.mode = AgentMode::ToolPending;
                self.advance_tool_queue(unknown_only_round).await;
            }
            AgentEvent::ToolDone {
                id,
                name,
                result,
                display_result,
                result_metadata,
                result_metadata_schema,
                summary,
                duration,
                content_sections,
                attachments,
            } => {
                self.current_tool_cancel = None;
                let allowance = tool_output_token_limit(
                    &name,
                    &self.context,
                    &self.registry.schemas(),
                    self.engine.effective_context_length(),
                    &queued_tool_names(&self.pending_tools, 0),
                );
                // `agent_finish` is only ever registered on a `Role::Child`
                // registry; once it completes the child's ReAct loop
                // exits unconditionally (exempt from the iteration
                // cap) — the tool itself already reported the
                // outcome to the parent via `finish_tx`.
                let is_finish = name == tools::agent_finish::AgentFinishTool::NAME;
                let result_section_id = if display_result.is_some() {
                    None
                } else {
                    Some(crate::tui_reducer_state::result_section_ids::HIDDEN.to_string())
                };
                self.context.push_tool_result(ToolResult {
                    id,
                    name: name.clone(),
                    result: result.clone(),
                    attachments,
                    result_section_id: result_section_id.clone(),
                    result_metadata,
                    result_metadata_schema,
                    summary: summary.clone(),
                    content_sections: content_sections.clone(),
                    duration,
                });
                if let Some(allowance) = allowance {
                    self.context.limit_last_model_message(allowance);
                }
                let _ = self
                    .ui_tx
                    .send(UiUpdate::ToolDone {
                        id,
                        name,
                        result_section: crate::tui_reducer_state::ResultSection {
                            id: result_section_id
                                .as_deref()
                                .unwrap_or(crate::tui_reducer_state::result_section_ids::RESULT)
                                .to_string(),
                            content: display_result.unwrap_or_default(),
                        },
                        summary,
                        duration,
                        content_sections,
                        run_id: self.own_run_id,
                    })
                    .await;
                if is_finish {
                    return LoopControl::Finished;
                }
                if self.compaction.schedule_summary(
                    &self.context,
                    self.engine.as_ref(),
                    &self.registry.schemas(),
                    self.generation,
                ) {
                    let _ = self
                        .ui_tx
                        .send(UiUpdate::Notice {
                            text: "Started background context compaction.".to_string(),
                            run_id: self.own_run_id,
                        })
                        .await;
                }
                self.advance_tool_queue(true).await;
            }
            AgentEvent::ToolError {
                id,
                name,
                error,
                partial_output,
                duration,
                content_sections,
            } => {
                self.current_tool_cancel = None;
                let allowance = tool_output_token_limit(
                    &name,
                    &self.context,
                    &self.registry.schemas(),
                    self.engine.effective_context_length(),
                    &queued_tool_names(&self.pending_tools, 0),
                );
                tracing::error!(
                    tool = %name,
                    tool_call_id = %id,
                    error = %error,
                    "tool failed"
                );
                self.context.push_tool_error(
                    id,
                    name,
                    error.clone(),
                    partial_output.as_deref(),
                    duration,
                    content_sections.clone(),
                );
                if let Some(allowance) = allowance {
                    self.context.limit_last_model_message(allowance);
                }
                let _ = self
                    .ui_tx
                    .send(UiUpdate::ToolError {
                        id,
                        error,
                        duration,
                        content_sections,
                        run_id: self.own_run_id,
                    })
                    .await;
                if self.compaction.schedule_summary(
                    &self.context,
                    self.engine.as_ref(),
                    &self.registry.schemas(),
                    self.generation,
                ) {
                    let _ = self
                        .ui_tx
                        .send(UiUpdate::Notice {
                            text: "Started background context compaction.".to_string(),
                            run_id: self.own_run_id,
                        })
                        .await;
                }
                self.advance_tool_queue(true).await;
            }
            AgentEvent::ToolKilled { id, name: _ } => {
                tracing::info!(%id, "tool killed; agent returning to idle");
                self.current_tool_cancel = None;
                self.pending_tools.clear();
                let _ = self
                    .ui_tx
                    .send(UiUpdate::ToolKilled {
                        id,
                        run_id: self.own_run_id,
                    })
                    .await;
                self.mode = AgentMode::Idle;
            }
        }
        LoopControl::CheckCompletion
    }
}
