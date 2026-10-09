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
use runtime::compaction_updates::send_context_usage_estimate;

fn is_context_window_rejection(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("context_length_exceeded")
        || message.contains("maximum context length")
        || message.contains("context window exceeded")
        || message.contains("exceeds the context window")
        || message.contains("prompt is too long")
}

async fn send_preflight_notice(
    sender: &mpsc::Sender<(u64, AgentEvent)>,
    generation: u64,
    text: String,
) -> bool {
    let (acknowledgement, received) = tokio::sync::oneshot::channel();
    let acknowledgement = Arc::new(Mutex::new(Some(acknowledgement)));
    if sender
        .send((
            generation,
            AgentEvent::PreflightNotice {
                text,
                acknowledgement,
            },
        ))
        .await
        .is_err()
    {
        return false;
    }
    received.await.is_ok()
}

/// Spawns the inner streaming task for one request, built from an immutable
/// snapshot of the conversation history. Returns the `CancellationToken` that
/// the outer loop uses to abort this request.
///
/// This task owns the [`ToolCallAccumulator`]s and assembles raw
/// [`StreamEvent::Delta`] fragments into a batch of [`AssembledToolCall`]s;
/// the outer loop stays the sole writer of
/// `Context`, and this task never writes it.
#[expect(clippy::too_many_arguments)]
pub(super) fn start_request(
    context: &mut Context,
    compaction: Option<&mut compaction::CompactionRuntime>,
    engine: Arc<dyn Engine>,
    tools: Vec<ToolSchema>,
    ui_tx: mpsc::Sender<UiUpdate>,
    agent_tx: mpsc::Sender<(u64, AgentEvent)>,
    generation: u64,
    own_run_id: Option<Uuid>,
    session_id: Option<Uuid>,
) -> CancellationToken {
    let cancel = CancellationToken::new();
    let mut notices = Vec::new();
    let preflight_context_length = engine.effective_context_length();
    let capacity_warning = compaction
        .as_ref()
        .map(|compaction_state| compaction_state.capacity_warning(engine.clone()));
    let send_notices = |notices: Vec<String>| {
        let sender = agent_tx.clone();
        tokio::spawn(
            async move {
                for notice in notices {
                    if !send_preflight_notice(&sender, generation, notice).await {
                        return;
                    }
                }
            }
            .in_current_span(),
        );
    };
    if let Some(compaction) = compaction {
        if compaction.defer_request(context, engine.as_ref(), &tools) {
            send_notices(vec!["Waiting for the running context summary within its original deadline; queued work has not been replayed.".to_string()]);
            return cancel;
        }
        let result = compaction.sweep(context, engine.as_ref(), &tools, generation);
        if result.summary_applied || result.prefix_dropped {
            let text = if result.summary_applied {
                "Background context compaction completed. Older context was summarized; the full transcript is preserved."
            } else {
                "Reduced context lossily before continuation. The task anchor and full transcript are preserved."
            };
            notices.push(text.to_string());
        }
        if compaction.defer_request(context, engine.as_ref(), &tools) {
            notices.push(
                "Waiting for the running context summary within its original deadline.".to_string(),
            );
            send_notices(notices);
            return cancel;
        }
    }
    let retrying = context.take_context_rejection_retry();
    let ceiling = compaction::safe_request_ceiling(engine.effective_context_length());
    let ceiling = if retrying { ceiling * 3 / 4 } else { ceiling };
    if context.estimated_model_tokens(&tools, engine.as_ref()) > ceiling {
        context.set_recovery_trigger(retrying);
        let report = context.recover_model_view(
            (engine.effective_context_length() as f64 * engine.config().compact_target_ratio)
                as usize,
            ceiling,
            &tools,
        );
        let sender = agent_tx.clone();
        if !report.fits_ceiling {
            tracing::error!(
                estimated_tokens = report.estimated_tokens_after,
                ceiling,
                "no bounded continuation can fit"
            );
            tokio::spawn(async move {
                for notice in notices {
                    if !send_preflight_notice(&sender, generation, notice).await {
                        return;
                    }
                }
                let _ = sender.send((generation, AgentEvent::StreamError(
                    "The context window cannot hold the system instructions, tool definitions and a valid task continuation. Select a model with a larger context window or start a new conversation with a shorter task.".to_string()
                ))).await;
            }.in_current_span());
            return cancel;
        }
        notices.push("Reduced the model view lossily to fit the context window. The task anchor remains; omitted output and the full transcript are preserved.".to_string());
    }
    let messages = context.model_messages();
    context.persist_model_projection();
    let request_tools = tools.clone();
    let usage_update_sender = ui_tx.clone();
    let on_request_estimate = Box::new(move |estimated_tokens, request_context_length| {
        let usage_update_sender = usage_update_sender.clone();
        Box::pin(async move {
            send_context_usage_estimate(
                &usage_update_sender,
                estimated_tokens,
                request_context_length,
                own_run_id,
                ContextUsageSource::SerializedRequest,
            )
            .await;
        }) as futures::future::BoxFuture<'static, ()>
    });
    let request_dump_failure_handler = crate::http::RequestDumpFailureHandler::default();
    let inner_cancel = cancel.clone();
    // `run_id` is `None` for the root loop and `Some(...)` for a child loop
    // (see `AgentLoop::own_run_id`); attaching it to this span lets every
    // nested log emitted while this request is in flight — including the
    // provider-level "chat request started"/"stream ended with [DONE]"
    // lines in `llm::openai_compatible_provider` — identify which agent loop it
    // belongs to, without threading `run_id` through the provider itself.
    let span = tracing::info_span!("agent_stream", run_id = ?own_run_id);
    tokio::spawn(
        async move {
            if let Some(capacity_warning) = capacity_warning {
                if let Some(notice) = capacity_warning.await {
                    tracing::warn!(%notice, "compaction engine capacity may be insufficient");
                    if !send_preflight_notice(&agent_tx, generation, notice).await {
                        return;
                    }
                }
            }
            for notice in notices {
                if !send_preflight_notice(&agent_tx, generation, notice).await {
                    return;
                }
            }
            if inner_cancel.is_cancelled() {
                tracing::debug!("session request cancelled");
                let _ = ui_tx.send(UiUpdate::StreamCancelled).await;
                let _ = agent_tx
                    .send((
                        generation,
                        AgentEvent::StreamCancelled {
                            partial: String::new(),
                        },
                    ))
                    .await;
                return;
            }
            if engine.effective_context_length() != preflight_context_length {
                let _ = agent_tx
                    .send((generation, AgentEvent::ContextWindowChanged))
                    .await;
                return;
            }
            let mut stream = match engine
                .chat_with_session_id_and_diagnostics(
                    messages,
                    request_tools,
                    inner_cancel.clone(),
                    session_id,
                    on_request_estimate,
                    Some(request_dump_failure_handler),
                )
                .await
            {
                Ok(stream) => stream,
                Err(error) => {
                    // If the cancel token was already fired (e.g. user aborted
                    // while the server was still thinking), treat the resulting
                    // error as a cancellation rather than a hard error so
                    // domain_aborting receives StreamCancelled and exits cleanly.
                    if inner_cancel.is_cancelled() {
                        tracing::debug!(
                            "engine chat failed after cancel; treating as StreamCancelled"
                        );
                        let _ = ui_tx.send(UiUpdate::StreamCancelled).await;
                        let _ = agent_tx
                            .send((
                                generation,
                                AgentEvent::StreamCancelled {
                                    partial: String::new(),
                                },
                            ))
                            .await;
                    } else {
                        let message = format!("{error}");
                        tracing::debug!(
                            message = %message,
                            "engine chat failed; forwarding stream failure"
                        );
                        let event = if !retrying && is_context_window_rejection(&message) {
                            AgentEvent::ContextWindowRejected(message)
                        } else {
                            AgentEvent::StreamError(message)
                        };
                        let _ = agent_tx.send((generation, event)).await;
                    }
                    return;
                }
            };
            // `engine.chat` completed the one-shot catalog hydration, so any
            // notices it queued (e.g. a dropped profile reasoning effort) are
            // ready — surface them before the stream events so they render
            // above this turn's response.
            for text in engine.take_pending_notices() {
                let _ = ui_tx
                    .send(UiUpdate::Notice {
                        text,
                        run_id: own_run_id,
                    })
                    .await;
            }
            let _ = ui_tx.send(UiUpdate::StreamStarted).await;
            let mut partial = String::new();
            let mut accumulators: std::collections::BTreeMap<usize, ToolCallAccumulator> =
                std::collections::BTreeMap::new();
            while let Some(event) = stream.recv().await {
                match event {
                    StreamEvent::Text(text) => {
                        partial.push_str(&text);
                        let _ = ui_tx.send(UiUpdate::AppendText(text)).await;
                    }
                    StreamEvent::Thinking(text) => {
                        // Never appended to `partial` — thinking content must
                        // not affect the assistant text or tool-call logic.
                        let _ = ui_tx.send(UiUpdate::AppendThinking(text)).await;
                    }
                    StreamEvent::Delta(delta) => {
                        if !accumulators.contains_key(&delta.tool_call_index)
                            && accumulators.len() >= MAX_TOOL_CALLS_PER_RESPONSE
                        {
                            inner_cancel.cancel();
                            let _ = agent_tx
                                .send((
                                    generation,
                                    AgentEvent::ToolCallLimitExceeded {
                                        limit: MAX_TOOL_CALLS_PER_RESPONSE,
                                    },
                                ))
                                .await;
                            break;
                        }
                        let accumulator =
                            accumulators
                                .entry(delta.tool_call_index)
                                .or_insert_with(|| ToolCallAccumulator {
                                    id: Uuid::new_v4(),
                                    name: None,
                                    arguments: String::new(),
                                });
                        if let Some(name) = delta.function_name {
                            if accumulator.name.is_none() {
                                accumulator.name = Some(name);
                            }
                        }
                        if let Some(fragment) = delta.arguments_fragment {
                            accumulator.arguments.push_str(&fragment);
                        }
                    }
                    StreamEvent::Done => {
                        if accumulators.is_empty() {
                            let _ = agent_tx
                                .send((generation, AgentEvent::StreamDone { partial }))
                                .await;
                        } else {
                            let mut calls = Vec::new();
                            let mut error: Option<String> = None;
                            for (_, accumulator) in accumulators {
                                match accumulator.into_tool_call_checked() {
                                    Ok(call) => calls.push(call),
                                    Err(raw) => {
                                        error = Some(format!(
                                            "stream ended with incomplete tool-call JSON: {raw}"
                                        ));
                                        break;
                                    }
                                }
                            }
                            if let Some(error) = error {
                                let _ = agent_tx
                                    .send((generation, AgentEvent::StreamError(error)))
                                    .await;
                            } else {
                                let _ = agent_tx
                                    .send((generation, AgentEvent::ToolCall { calls }))
                                    .await;
                            }
                        }
                        break;
                    }
                    StreamEvent::Cancelled => {
                        if accumulators.is_empty() {
                            let _ = ui_tx.send(UiUpdate::StreamCancelled).await;
                        }
                        let _ = agent_tx
                            .send((generation, AgentEvent::StreamCancelled { partial }))
                            .await;
                        break;
                    }
                    StreamEvent::Error(error) => {
                        let message = format!("{error}");
                        let event = if !retrying
                            && partial.is_empty()
                            && accumulators.is_empty()
                            && is_context_window_rejection(&message)
                        {
                            AgentEvent::ContextWindowRejected(message)
                        } else {
                            AgentEvent::StreamError(message)
                        };
                        let _ = agent_tx.send((generation, event)).await;
                        break;
                    }
                }
            }
            if inner_cancel.is_cancelled() {
                tracing::debug!("session request cancelled");
            }
        }
        .instrument(span),
    );
    cancel
}

/// Buffers the fragmented pieces of one tool call.
///
/// Raw `Delta` fragments for a given `tool_call_index` are concatenated in
/// order; the name is captured from the first fragment that carries it and the
/// accumulated `arguments` JSON is only parsed once complete.
#[derive(Debug, Clone)]
pub(super) struct ToolCallAccumulator {
    pub(super) id: Uuid,
    pub(super) name: Option<String>,
    pub(super) arguments: String,
}

impl ToolCallAccumulator {
    /// Finalize this accumulator into a validated tool call, or return the raw
    /// argument fragment when the JSON is incomplete/unparseable.
    pub(super) fn into_tool_call_checked(self) -> Result<AssembledToolCall, String> {
        let name = self.name.unwrap_or_default();
        if self.arguments.trim().is_empty() {
            return Ok(AssembledToolCall {
                id: self.id,
                name,
                arguments: Value::Null,
            });
        }
        match serde_json::from_str(&self.arguments) {
            Ok(arguments) => Ok(AssembledToolCall {
                id: self.id,
                name,
                arguments,
            }),
            Err(_) => Err(self.arguments),
        }
    }
}
