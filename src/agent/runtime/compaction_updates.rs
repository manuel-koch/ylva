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

/// Place a newly applied summary marker at the corresponding replay entry.
pub(in crate::agent) async fn send_compaction_applied(
    ui_tx: &mpsc::Sender<UiUpdate>,
    context: &Context,
    boundary: usize,
    run_id: Option<Uuid>,
) {
    let entry_index =
        crate::tui_reducer_state::project_messages(&context.messages()[..boundary]).len();
    let summarized_messages = boundary.saturating_sub(context.leading_system_len());
    let _ = ui_tx
        .send(UiUpdate::CompactionApplied {
            run_id,
            entry_index,
            summarized_messages,
        })
        .await;
}

/// Run one compaction sweep at a safe boundary and report the outcome: the
/// compaction lifecycle notices and a transcript marker when that sweep
/// applied a snapshot the transcript has not shown yet.
///
/// The replayed marker comes from the persisted snapshot instead (the TUI
/// reads it before the loop ever runs), so only a compaction applied *here*
/// emits this update — `boundary_before`/`boundary_after` make the sweep's
/// decision visible to the caller.
pub(in crate::agent) async fn sweep_compaction_and_report(
    compaction: &mut compaction::CompactionRuntime,
    context: &mut Context,
    engine: &dyn Engine,
    tools: &[ToolSchema],
    generation: u64,
    run_id: Option<Uuid>,
    ui_tx: &mpsc::Sender<UiUpdate>,
) -> bool {
    let boundary_before = context.compaction().map(|snapshot| snapshot.boundary);
    let result = compaction.sweep(context, engine, tools, generation);
    if let Some(outcome) = &result.manual_outcome {
        let (text, request_id) = match outcome {
            compaction::ManualCompactionResult::Applied { request_id } => (
                "Manual context compaction completed. The latest task and following messages \
                 remain in the model view; the full transcript is preserved.",
                *request_id,
            ),
            compaction::ManualCompactionResult::NoReduction { request_id } => (
                "No reduction from manual compaction; the model view is unchanged.",
                *request_id,
            ),
            compaction::ManualCompactionResult::Ineligible { request_id } => (
                "Manual compaction could not safely apply; the model view is unchanged.",
                *request_id,
            ),
        };
        report_manual_compaction_notice(ui_tx, run_id, request_id, text).await;
    } else if result.summary_applied {
        let _ = ui_tx
            .send(UiUpdate::Notice {
                text: "Background context compaction completed. Older context was summarized; \
                       the full transcript is preserved. If compaction becomes frequent, \
                       consider narrowing the task or starting a new conversation with a \
                       concise handoff, as repeated compaction may lose detail."
                    .to_string(),
                run_id,
            })
            .await;
    }
    if result.prefix_dropped {
        let _ = ui_tx
            .send(UiUpdate::Notice {
                text: "Compacted context synchronously to fit the context window. \
                       Older context was removed from the model view; some details may no \
                       longer be available to the model. The full transcript is preserved."
                    .to_string(),
                run_id,
            })
            .await;
    }
    if result.summary_started {
        let _ = ui_tx
            .send(UiUpdate::Notice {
                text: "Started background context compaction.".to_string(),
                run_id,
            })
            .await;
    }
    if result.prefix_dropped || result.summary_applied {
        context.persist_model_projection();
        send_context_usage_estimate(
            ui_tx,
            context.estimated_model_tokens(tools, engine),
            engine.effective_context_length(),
            run_id,
            ContextUsageSource::ModelView,
        )
        .await;
    }
    if !compaction.enabled() {
        return result.prefix_dropped;
    }
    if let Some(boundary_after) = context.compaction().map(|snapshot| snapshot.boundary) {
        if boundary_before != Some(boundary_after) {
            send_compaction_applied(ui_tx, context, boundary_after, run_id).await;
        }
    }
    result.prefix_dropped || result.summary_applied
}

pub(in crate::agent) async fn report_manual_compaction_notice(
    ui_tx: &mpsc::Sender<UiUpdate>,
    run_id: Option<Uuid>,
    request_id: Option<Uuid>,
    text: &str,
) {
    tracing::debug!(?run_id, ?request_id, "manual compaction notice reported");
    let _ = ui_tx
        .send(UiUpdate::Notice {
            text: text.to_string(),
            run_id,
        })
        .await;
}

pub(in crate::agent) async fn send_context_usage_estimate(
    ui_tx: &mpsc::Sender<UiUpdate>,
    estimated_tokens: usize,
    context_length: usize,
    run_id: Option<Uuid>,
    source: ContextUsageSource,
) {
    tracing::debug!(
        run_id = ?run_id,
        estimated_tokens,
        context_length,
        usage_ratio = if context_length == 0 {
            0.0
        } else {
            estimated_tokens as f64 / context_length as f64
        },
        source = ?source,
        "context usage estimate reported"
    );
    let update = match run_id {
        Some(run_id) => UiUpdate::ChildContextUsage {
            run_id,
            estimated_tokens,
            context_length,
            source,
        },
        None => UiUpdate::ContextUsage {
            estimated_tokens,
            context_length,
            source,
        },
    };
    let _ = ui_tx.send(update).await;
}

pub(in crate::agent) async fn report_compaction_failure(
    ui_tx: &mpsc::Sender<UiUpdate>,
    run_id: Option<Uuid>,
    reason: &str,
) {
    let _ = ui_tx
        .send(UiUpdate::Notice {
            text: format!("Background context compaction failed: {reason}."),
            run_id,
        })
        .await;
}

/// Awaits the next `FinishResult` when `rx` is `Some`, else never resolves.
/// Lets `AgentLoop::run`'s `select!` treat "no `ChildAgentManager` wired" the
/// same as "channel open but empty" without an `Option`-shaped branch.
pub(super) async fn recv_finish(
    rx: &mut Option<mpsc::UnboundedReceiver<crate::child_agent_manager::FinishResult>>,
) -> Option<crate::child_agent_manager::FinishResult> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// Awaits the next forwarded child `UiUpdate` when `rx` is `Some`, else
/// never resolves. See [`recv_finish`].
pub(super) async fn recv_subagent_ui(
    rx: &mut Option<mpsc::UnboundedReceiver<UiUpdate>>,
) -> Option<UiUpdate> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// Awaits cancellation of `token` when `token` is `Some`, else never
/// resolves. See [`recv_finish`].
pub(super) async fn cancelled_or_pending(token: &Option<CancellationToken>) {
    match token {
        Some(token) => token.cancelled().await,
        None => std::future::pending().await,
    }
}
