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
use crate::agent::protocol::agent_command_name;

impl RunState {
    async fn reset_compaction(&mut self) {
        let manual_pending = self.compaction.trigger() == compaction::CompactionTrigger::Manual
            && self.compaction.summary_pending();
        let request_id = self.compaction.request_id();
        self.compaction.reset().await;
        if manual_pending {
            compaction_updates::report_manual_compaction_notice(
                &self.ui_tx,
                self.own_run_id,
                request_id,
                "Manual context compaction cancelled; the model view is unchanged.",
            )
            .await;
        }
    }

    pub(super) async fn handle_command(&mut self, cmd: Option<AgentCommand>) -> LoopControl {
        let Some(cmd) = cmd else {
            // AgentLoop dropped → cmd_tx closed → clean shutdown:
            // return the final committed Context (read-only history).
            return LoopControl::Finished;
        };
        log_agent_command(
            &cmd,
            self.mode,
            self.current_cancel.is_some(),
            self.current_tool_cancel.is_some(),
        );
        if self.session_switch_frozen
            && matches!(
                &cmd,
                AgentCommand::Prompt(_)
                    | AgentCommand::ClearContext
                    | AgentCommand::Compact
                    | AgentCommand::RecordModelChanged { .. }
            )
        {
            tracing::warn!(
                command = agent_command_name(&cmd),
                "agent mutation rejected by session replacement barrier"
            );
            let _ = self
                .ui_tx
                .send(UiUpdate::Notice {
                    text: "Session switch in progress; this command was not accepted.".to_string(),
                    run_id: self.own_run_id,
                })
                .await;
            return LoopControl::PollAgain;
        }
        match cmd {
            AgentCommand::PrepareSessionSwitch { reply } => {
                let accepted = self.session_switch_quiescent();
                if accepted {
                    self.session_switch_frozen = true;
                }
                tracing::debug!(accepted, "session replacement barrier checked");
                reply.send(accepted);
                return LoopControl::PollAgain;
            }
            AgentCommand::CancelSessionSwitch => {
                self.session_switch_frozen = false;
                return LoopControl::PollAgain;
            }
            AgentCommand::Shutdown => {
                if self.session_switch_frozen && self.session_switch_quiescent() {
                    return LoopControl::Finished;
                }
                tracing::warn!("shutdown rejected without a quiescent session replacement barrier");
                return LoopControl::PollAgain;
            }
            AgentCommand::Compact => {
                if self.is_child {
                    let _ = self
                        .ui_tx
                        .send(UiUpdate::Notice {
                            text: "Manual compaction is available only in the root conversation."
                                .to_string(),
                            run_id: self.own_run_id,
                        })
                        .await;
                    return LoopControl::PollAgain;
                }
                let outcome = self.compaction.schedule_manual_summary(
                    &self.context,
                    self.engine.as_ref(),
                    self.generation,
                );
                let (text, request_id) = match &outcome {
                    compaction::ManualCompactionSchedule::Started { request_id } => {
                        ("Started manual context compaction.", Some(*request_id))
                    }
                    compaction::ManualCompactionSchedule::AlreadyPending { request_id } => (
                        "Already compacting; no additional summary was queued.",
                        *request_id,
                    ),
                    compaction::ManualCompactionSchedule::NothingEligible => (
                        "Nothing eligible for manual compaction; the latest task is retained.",
                        None,
                    ),
                    compaction::ManualCompactionSchedule::Unavailable => (
                        "Manual compaction is unavailable for this conversation.",
                        None,
                    ),
                };
                tracing::info!(?request_id, ?outcome, "manual compaction command evaluated");
                compaction_updates::report_manual_compaction_notice(
                    &self.ui_tx,
                    self.own_run_id,
                    request_id,
                    text,
                )
                .await;
                return LoopControl::PollAgain;
            }
            AgentCommand::Prompt(text) => {
                if text.is_empty() {
                    return LoopControl::PollAgain;
                }
                if self.mode == AgentMode::Idle {
                    // A fresh turn is a safe boundary too:
                    // applying (or forcing) compaction here
                    // keeps the request that follows inside
                    // the active context window.
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
                    self.iterations = 1;
                    self.context.push_user(text);
                    self.mode = AgentMode::Streaming;
                    if self.startup_checks_finished {
                        self.start_request();
                    } else {
                        self.request_waiting_for_startup_checks = true;
                    }
                } else {
                    // Cooperative steering: never cancel the live
                    // stream. Queued from ToolPending too; injection
                    // is deferred (Phase 4 adds descope_pending_tools).
                    self.pending_steers.push_back(text);
                    let _ = self
                        .ui_tx
                        .send(UiUpdate::SteerQueued {
                            run_id: self.own_run_id,
                        })
                        .await;
                }
            }
            AgentCommand::ApproveTool { id } => {
                // Only valid in `ToolPending`; ignored otherwise.
                let is_front = self.mode == AgentMode::ToolPending
                    && self
                        .pending_tools
                        .front()
                        .is_some_and(|pending| pending.call.id() == id);
                if is_front {
                    let allowance = tool_output_token_limit(
                        self.pending_tools
                            .front()
                            .expect("checked front")
                            .call
                            .name(),
                        &self.context,
                        &self.registry.schemas(),
                        self.engine.effective_context_length(),
                        &queued_tool_names(&self.pending_tools, 1),
                    );
                    if allowance.is_some_and(|tokens| tokens < MINIMUM_USEFUL_RESULT_TOKENS) {
                        self.advance_tool_queue(true).await;
                        return LoopControl::PollAgain;
                    }
                    let pending = self.pending_tools.pop_front().expect("checked front");
                    if pending.call.name() == tools::spawn_subagent::SpawnSubagentTool::NAME {
                        resolve_spawn_subagent(
                            pending,
                            &mut self.child_agent_manager,
                            &mut self.context,
                            &self.ui_tx,
                            &mut self.spawn_content_sections,
                        )
                        .await;
                        self.advance_tool_queue(true).await;
                    } else {
                        let name = pending.call.name().to_string();
                        let call_id = pending.call.id();
                        let arguments = pending.call.arguments().clone();
                        let content_sections = pending.call.pending_content_sections();
                        let title_arguments = pending.call.title_arguments();
                        tracing::info!(tool = %name, tool_call_id = %call_id, "tool approved");
                        let max_output_tokens = tool_output_token_limit(
                            &name,
                            &self.context,
                            &self.registry.schemas(),
                            self.engine.effective_context_length(),
                            &queued_tool_names(&self.pending_tools, 0),
                        );
                        self.current_tool_cancel = Some(PendingTool::execute(
                            pending,
                            self.agent_tx.clone(),
                            self.generation,
                            max_output_tokens,
                        ));
                        let _ = self
                            .ui_tx
                            .send(UiUpdate::ToolStart {
                                name: name.clone(),
                                id: call_id,
                                arguments: arguments.clone(),
                                content_sections,
                                title_arguments,
                                run_id: self.own_run_id,
                            })
                            .await;
                    }
                }
            }
            AgentCommand::DenyTool { id, reason } => {
                // Only valid in `ToolPending`; ignored otherwise.
                let is_front = self.mode == AgentMode::ToolPending
                    && self
                        .pending_tools
                        .front()
                        .is_some_and(|pending| pending.call.id() == id);
                if is_front {
                    let denied = self.pending_tools.pop_front().expect("checked front");
                    let name = denied.call.name().to_string();
                    let call_id = denied.call.id();
                    let denied_content_sections = denied.call.pending_content_sections();
                    tracing::info!(tool = %name, tool_call_id = %call_id, "tool denied");
                    let mut error = format!("Tool '{name}' was denied by the user");
                    if !reason.is_empty() {
                        error.push_str(&format!(
                            ". Here is their reason for the denial:\n\n{reason}"
                        ));
                    }
                    self.context.push_tool_error(
                        call_id,
                        name.clone(),
                        error.clone(),
                        None,
                        std::time::Duration::ZERO,
                        denied_content_sections.clone(),
                    );
                    // When the user supplied a reason, also push a
                    // `user` role message so the model reads the
                    // correction through the authoritative instruction
                    // channel rather than just the tool-error envelope.
                    if !reason.is_empty() {
                        self.context.push_implicit_user(format!(
                            "I denied the '{name}' tool call \
                                                 ( tool_call_id={call_id} ). \
                                                 Here is my reason for the denial:\n\n{reason}"
                        ));
                    }
                    let _ = self
                        .ui_tx
                        .send(UiUpdate::ToolError {
                            id: call_id,
                            error,
                            duration: std::time::Duration::ZERO,
                            content_sections: denied_content_sections,
                            run_id: self.own_run_id,
                        })
                        .await;
                    self.advance_tool_queue(true).await;
                }
            }
            AgentCommand::AnswerClarify(answer) => {
                let is_front = self.mode == AgentMode::ClarifyPending
                    && self
                        .pending_tools
                        .front()
                        .is_some_and(|pending| pending.call.name() == "clarify");
                if is_front {
                    let pending = self.pending_tools.pop_front().expect("checked front");
                    let call_id = pending.call.id();
                    let choices_sections = tools::clarify::ClarifyTool::answer_display_sections(
                        pending.call.arguments(),
                    );
                    let duration = self
                        .clarify_started_at
                        .take()
                        .map(|started| started.elapsed())
                        .unwrap_or(std::time::Duration::ZERO);
                    self.context.push_tool_result(ToolResult {
                        id: call_id,
                        name: tools::clarify::ClarifyTool::NAME.to_string(),
                        result: answer.clone(),
                        attachments: vec![],
                        result_section_id: Some(
                            crate::tui_reducer_state::result_section_ids::USER_ANSWER.to_string(),
                        ),
                        result_metadata: None,
                        result_metadata_schema: None,
                        summary: vec![],
                        content_sections: choices_sections.clone(),
                        duration,
                    });
                    let _ = self
                        .ui_tx
                        .send(UiUpdate::ToolDone {
                            id: call_id,
                            name: tools::clarify::ClarifyTool::NAME.to_string(),
                            result_section: crate::tui_reducer_state::ResultSection {
                                id: crate::tui_reducer_state::result_section_ids::USER_ANSWER
                                    .to_string(),
                                content: answer,
                            },
                            summary: vec![],
                            duration,
                            content_sections: choices_sections,
                            run_id: self.own_run_id,
                        })
                        .await;
                    self.mode = AgentMode::ToolPending;
                    self.advance_tool_queue(true).await;
                }
            }
            AgentCommand::AbortClarify => {
                let is_front = self.mode == AgentMode::ClarifyPending
                    && self
                        .pending_tools
                        .front()
                        .is_some_and(|pending| pending.call.name() == "clarify");
                if is_front {
                    let pending = self.pending_tools.pop_front().expect("checked front");
                    let call_id = pending.call.id();
                    let error = "User aborted the clarify question without answering.".to_string();
                    self.context.push_tool_error(
                        call_id,
                        tools::clarify::ClarifyTool::NAME.to_string(),
                        error.clone(),
                        None,
                        std::time::Duration::ZERO,
                        vec![],
                    );
                    let _ = self
                        .ui_tx
                        .send(UiUpdate::ToolError {
                            id: call_id,
                            error,
                            duration: std::time::Duration::ZERO,
                            content_sections: vec![],
                            run_id: self.own_run_id,
                        })
                        .await;
                    self.mode = AgentMode::ToolPending;
                    self.advance_tool_queue(true).await;
                }
            }
            AgentCommand::Stop { reason } => {
                self.reset_compaction().await;
                if let Some(manager) = self.child_agent_manager.as_mut() {
                    let cancelled_spawns = manager.cancel_all().await;
                    let cancelled_count = cancelled_spawns.len();
                    for (index, cancelled) in cancelled_spawns.into_iter().enumerate() {
                        // Store integration (child_result JSONL
                        // record) lands in Phase 6.11; for now
                        // the cancellation is only logged, but
                        // the tool bubble/context still get a
                        // proper terminal state below instead
                        // of being left stuck mid-flight.
                        tracing::info!(
                            run_id = %cancelled.run_id,
                            tool_call_id = %cancelled.call_id,
                            "child agent cancelled on parent stop"
                        );
                        self.cancelled_run_ids.insert(cancelled.run_id);
                        let mut content_sections = self
                            .spawn_content_sections
                            .remove(&cancelled.call_id)
                            .unwrap_or_default();
                        content_sections.push(tools::Section::Content(tools::ContentSection {
                            label: Some("Result".to_string()),
                            content:
                                "Cancelled: the parent agent stopped before the child finished."
                                    .to_string(),
                            file_extension: Some("md".to_string()),
                            line_number_offset: None,
                        }));
                        let cancellation_result = crate::child_agent_manager::FinishResult {
                            run_id: cancelled.run_id,
                            call_id: cancelled.call_id,
                            origin: crate::child_agent_manager::FinishResultOrigin::Runtime,
                            status: "cancelled".to_string(),
                            result: None,
                            error: Some(
                                "The parent agent stopped before the child finished.".to_string(),
                            ),
                            sources: vec![],
                        };
                        let _ = self
                            .ui_tx
                            .send(UiUpdate::ChildAgentDone {
                                run_id: cancelled.run_id,
                                status: cancellation_result.status.clone(),
                                result: None,
                            })
                            .await;
                        let outstanding_count = cancelled_count.saturating_sub(index + 1);
                        let result_text = serde_json::to_string(
                            &cancellation_result
                                .to_completion_json(&cancelled.label, outstanding_count),
                        )
                        .unwrap_or_default();
                        self.context.push_async_harness_notice(format!(
                                                "Subagent completion event for \
                                                 `{tool_name}`; the structured JSON is harness \
                                                 metadata, not a user-facing message. The parent \
                                                 stop cancelled this child, so no continuation will \
                                                 be started:\n{result_text}",
                                                tool_name = tools::spawn_subagent::SpawnSubagentTool::NAME,
                                            ));
                        let _ = self
                            .ui_tx
                            .send(UiUpdate::ToolDone {
                                id: cancelled.call_id,
                                name: tools::spawn_subagent::SpawnSubagentTool::NAME.to_string(),
                                result_section: crate::tui_reducer_state::ResultSection::plain(
                                    String::new(),
                                ),
                                summary: vec![("status".to_string(), "cancelled".to_string())],
                                duration: std::time::Duration::ZERO,
                                content_sections,
                                run_id: self.own_run_id,
                            })
                            .await;
                    }
                }
                // Interrupt: hard-abort the current round.
                if std::mem::take(&mut self.request_waiting_for_startup_checks) {
                    self.context
                        .push_assistant(String::new(), AssistantStatus::Interrupted);
                    self.mode = AgentMode::Idle;
                    let _ = self.ui_tx.send(UiUpdate::StreamCancelled).await;
                }
                self.pending_steers.clear();
                if let Some(cancel_token) = self.current_cancel.take() {
                    cancel_token.cancel();
                }
                if let Some(tool_cancel) = self.current_tool_cancel.take() {
                    tool_cancel.cancel();
                }
                if self.mode == AgentMode::ClarifyPending {
                    if let Some(pending) = self.pending_tools.pop_front() {
                        let name = pending.call.name().to_string();
                        let id = pending.call.id();
                        let content_sections = pending.call.pending_content_sections();
                        let mut error = format!("Tool '{name}' was denied by the user");
                        if !reason.is_empty() {
                            error.push_str(&format!(
                                ". Here is their reason for the denial:\n\n{reason}"
                            ));
                        }
                        self.context.push_tool_error(
                            id,
                            name,
                            error.clone(),
                            None,
                            std::time::Duration::ZERO,
                            content_sections.clone(),
                        );
                        let _ = self
                            .ui_tx
                            .send(UiUpdate::ToolError {
                                id,
                                error,
                                duration: std::time::Duration::ZERO,
                                content_sections,
                                run_id: self.own_run_id,
                            })
                            .await;
                    }
                    for pending in self.pending_tools.iter() {
                        let name = pending.call.name().to_string();
                        let id = pending.call.id();
                        let mut error = format!("Tool '{name}' was denied by the user");
                        if !reason.is_empty() {
                            error.push_str(&format!(
                                ". Here is their reason for the denial:\n\n{reason}"
                            ));
                        }
                        self.context.push_tool_error(
                            id,
                            name,
                            error,
                            None,
                            std::time::Duration::ZERO,
                            pending.call.pending_content_sections(),
                        );
                    }
                    self.pending_tools.clear();
                    self.mode = AgentMode::Idle;
                } else if self.mode == AgentMode::ToolPending {
                    // Record a denial result for every pending
                    // tool so the model history stays consistent
                    // (every tool call must have a result).
                    // Only the front tool has a visible bubble;
                    // the rest are queued without one yet.
                    let mut first = true;
                    for pending in self.pending_tools.iter() {
                        let name = pending.call.name().to_string();
                        let id = pending.call.id();
                        let content_sections = pending.call.pending_content_sections();
                        let mut error = format!("Tool '{name}' was denied by the user");
                        if !reason.is_empty() {
                            error.push_str(&format!(
                                ". Here is their reason for the denial:\n\n{reason}"
                            ));
                        }
                        self.context.push_tool_error(
                            id,
                            name,
                            error.clone(),
                            None,
                            std::time::Duration::ZERO,
                            content_sections.clone(),
                        );
                        if first {
                            let _ = self
                                .ui_tx
                                .send(UiUpdate::ToolError {
                                    id,
                                    error,
                                    duration: std::time::Duration::ZERO,
                                    content_sections,
                                    run_id: self.own_run_id,
                                })
                                .await;
                            first = false;
                        }
                    }
                    self.pending_tools.clear();
                    self.mode = AgentMode::Idle;
                }
                if self.mode == AgentMode::Idle {
                    let _ = self.ui_tx.send(UiUpdate::AgentIdle).await;
                }
            }
            AgentCommand::ClearContext => {
                self.request_waiting_for_startup_checks = false;
                // Interrupt whatever was in flight before
                // swapping in `fresh_context`, mirroring
                // `AgentCommand::Stop`'s cleanup — otherwise
                // the old stream/tool/children keep running
                // against a `Context` nobody can see anymore
                // and their eventual `AgentEvent`s land in the
                // new, unrelated conversation instead (see
                // the `generation` bump below for the part
                // cancellation alone can't cover).
                self.generation = self.generation.wrapping_add(1);
                // A summary (pending or in flight) belongs to
                // the conversation being cleared; its result
                // must never shape the replacement's model
                // view. Abort the request and clear its activity.
                self.reset_compaction().await;
                self.pending_steers.clear();
                self.pending_tools.clear();
                if let Some(cancel_token) = self.current_cancel.take() {
                    cancel_token.cancel();
                }
                if let Some(tool_cancel) = self.current_tool_cancel.take() {
                    tool_cancel.cancel();
                }
                if let Some(manager) = self.child_agent_manager.as_mut() {
                    for cancelled in manager.cancel_all().await {
                        tracing::info!(
                            run_id = %cancelled.run_id,
                            tool_call_id = %cancelled.call_id,
                            "child agent cancelled on clear_context"
                        );
                        self.cancelled_run_ids.insert(cancelled.run_id);
                        self.spawn_content_sections.remove(&cancelled.call_id);
                    }
                }
                self.mode = AgentMode::Idle;
                let mut fresh_context = match self.system_prompt.as_deref() {
                    Some(supplied_system_prompt) => {
                        Context::with_system_prompt(supplied_system_prompt)
                    }
                    None => Context::new(),
                };
                if let Some(store) = &self.store_for_commands {
                    let config = self.engine.config();
                    match store.start_new_session(
                        &config.provider_connection,
                        &config.model,
                        None,
                        self.agent_type.as_deref(),
                    ) {
                        Ok(new_store) => fresh_context.set_store(new_store),
                        Err(error) => tracing::warn!(
                            %error,
                            "could not start a new session file; the cleared conversation is not persisted"
                        ),
                    }
                }
                self.context = fresh_context;
                self.session_id = self.context.store().map(|store| store.session_id());
                self.store_for_commands = self.context.store();
                if let Some(manager) = self.child_agent_manager.as_mut() {
                    manager.set_store(self.context.store());
                }
                *self
                    .current_store
                    .lock()
                    .expect("current store mutex is not poisoned") = self.context.store();
            }
            AgentCommand::RecordModelChanged {
                provider_connection,
                model,
            } => {
                // The common local baseline does not change
                // across provider/model switches.
                if let Some(store) = self.context.store() {
                    store.record_model_changed(&provider_connection, &model);
                }
                self.model_change_capacity_check = Some(Box::pin(
                    self.compaction.capacity_warning(self.engine.clone()),
                ));
                // The model view is unchanged, but the active window may have
                // changed: re-report the estimate against the new window.
                send_context_usage_estimate(
                    &self.ui_tx,
                    self.context
                        .estimated_model_tokens(&self.registry.schemas(), self.engine.as_ref()),
                    self.engine.effective_context_length(),
                    self.own_run_id,
                    ContextUsageSource::ModelView,
                )
                .await;
            }
            AgentCommand::ForChild { run_id, command } => {
                // Only a root loop's `child_agent_manager` can
                // resolve `run_id` to a live child command
                // channel; forward the inner command verbatim
                // — the child's own loop never sees `ForChild`
                // itself, only plain `ApproveTool`/`DenyTool`/
                // `AnswerClarify`/`AbortClarify` on its own
                // channel, exactly as if sent directly.
                let sender = self
                    .child_agent_manager
                    .as_ref()
                    .and_then(|manager| manager.running_command_sender(run_id));
                match sender {
                    Some(sender) => {
                        let _ = sender.send(*command).await;
                    }
                    None => {
                        tracing::warn!(
                            %run_id,
                            "AgentCommand::ForChild targets a run_id with no \
                             running child (already finished/cancelled); dropping"
                        );
                    }
                }
            }
        }
        LoopControl::CheckCompletion
    }
}
