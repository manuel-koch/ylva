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

use super::input::{
    clear_textarea, dedent, load_history_into_textarea, paste_text, restore_input_state,
    save_input_state_and_clear, set_input_text, MouseDrag,
};
use super::models::{apply_model_list_result, fetch_model_list, switch_model};
use super::{ActionOutcome, ClipboardSink};
use crate::agent::{AgentCommand, UiUpdate};
use crate::llm::engine::Engine;
use crate::run_outcome::RunOutcome;
use crate::tui_reducer_state::{AppState, Effect, SlashOverlay};
use ratatui_textarea::TextArea;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Driver-local tracking of which subagent (if any) raised each pending
/// tool/clarify approval. The Presenter/`AppState`/`Effect` surface stays
/// agent-agnostic (an `Effect::ApproveTool { id }` carries no `run_id`); this
/// struct is threaded separately through the Driver alongside `AppState` so
/// the resulting `AgentCommand` can still be routed correctly.
///
/// - `tool_run_ids` maps a pending tool call's `id` (as carried on
///   `UiUpdate::ToolPending`/`Effect::ApproveTool`/`Effect::DenyTool`) to the
///   subagent's `run_id` that raised it. Populated in `process_one_update` when
///   a `ToolPending` update carries `Some(run_id)`; consumed (removed) in
///   `execute_effects` when the corresponding `ApproveTool`/`DenyTool` effect
///   fires. A missing key means "root's own pending tool" — same meaning as
///   `run_id: None` on the originating `UiUpdate`.
/// - `clarify_run_ids` maps a pending clarify question's `id` to the `run_id`
///   (`None` for the root) that asked it. Several clarify questions can be
///   outstanding at once (the Presenter shows them one at a time), so the
///   map is keyed by `id`, which `Effect::AnswerClarify`/`AbortClarify`
///   carry. Set in `process_one_update` from `UiUpdate::ClarifyPending`;
///   consumed in `execute_effects` when `AnswerClarify`/`AbortClarify` fires.
#[derive(Debug, Default)]
pub(super) struct PendingRunIds {
    tool_run_ids: std::collections::HashMap<uuid::Uuid, uuid::Uuid>,
    clarify_run_ids: std::collections::HashMap<uuid::Uuid, Option<uuid::Uuid>>,
    /// Terminal facts for the exit code. Recorded here because this struct is
    /// already threaded through every update and effect path.
    pub(super) run_outcome: RunOutcome,
}

impl PendingRunIds {
    /// An external SIGINT or SIGTERM reached the TUI. It interrupts the run
    /// whether a turn is active or the TUI is idle, because the TUI quits on it.
    pub(super) fn record_stop_signal(&mut self) {
        self.run_outcome.record_interruption();
    }

    pub(super) fn record_update(&mut self, update: &UiUpdate) {
        if let UiUpdate::ToolPending {
            id,
            run_id: Some(run_id),
            ..
        } = update
        {
            self.tool_run_ids.insert(*id, *run_id);
        }
        if let UiUpdate::ClarifyPending { id, run_id, .. } = update {
            self.clarify_run_ids.insert(*id, *run_id);
        }
    }
}

/// Wraps `command` in `AgentCommand::ForSubagent { run_id, .. }` when `run_id`
/// is `Some`; otherwise returns `command` unchanged, exactly as sent today
/// for the root's own pending items.
fn route_command(run_id: Option<uuid::Uuid>, command: AgentCommand) -> AgentCommand {
    match run_id {
        Some(run_id) => AgentCommand::ForSubagent {
            run_id,
            command: Box::new(command),
        },
        None => command,
    }
}

/// Execute a batch of [`Effect`]s produced by the Presenter, performing the
/// actual I/O (channel sends, textarea mutations, clipboard writes).
/// Returns `ActionOutcome::Quit` if any `Effect::Quit` was processed.
pub(super) async fn execute_effects(
    state: &mut AppState,
    textarea: &mut TextArea<'_>,
    cmd_tx: &mpsc::Sender<AgentCommand>,
    effects: Vec<Effect>,
    mouse_drag: &mut MouseDrag,
    clipboard: &mut dyn ClipboardSink,
    pending_run_ids: &mut PendingRunIds,
) -> ActionOutcome {
    for effect in effects {
        match effect {
            Effect::SubmitPrompt { steer } => {
                // Only a fresh turn starts a new outcome. A steer joins the
                // running turn, so its outcome stays.
                if !steer {
                    pending_run_ids.run_outcome = RunOutcome::default();
                }
                let raw_text = textarea.lines().join("\n");
                let text = dedent(&raw_text);
                let _ = cmd_tx.try_send(AgentCommand::Prompt(text));
            }
            Effect::ClearTextarea => {
                clear_textarea(textarea);
                *mouse_drag = MouseDrag::default();
            }
            Effect::SaveInputStateAndClear => {
                save_input_state_and_clear(state, textarea);
            }
            Effect::RestoreInputState => {
                restore_input_state(state, textarea);
            }
            Effect::SetInputText(text) => {
                set_input_text(textarea, &text);
            }
            Effect::TextareaKey(key_event) => {
                textarea.input(key_event);
            }
            Effect::Paste(text) => {
                paste_text(state, textarea, &text);
            }
            Effect::ApproveTool { id } => {
                state.apply_tool_approved(id);
                let run_id = pending_run_ids.tool_run_ids.remove(&id);
                let _ = cmd_tx.try_send(route_command(run_id, AgentCommand::ApproveTool { id }));
            }
            Effect::DenyTool { id, reason } => {
                pending_run_ids.run_outcome.record_approval_denied();
                state.apply_tool_denied(id, &reason);
                let run_id = pending_run_ids.tool_run_ids.remove(&id);
                let _ =
                    cmd_tx.try_send(route_command(run_id, AgentCommand::DenyTool { id, reason }));
            }
            Effect::AnswerClarify { id, answer } => {
                let run_id = pending_run_ids.clarify_run_ids.remove(&id).flatten();
                let _ = cmd_tx.try_send(route_command(run_id, AgentCommand::AnswerClarify(answer)));
            }
            Effect::AbortClarify { id } => {
                let run_id = pending_run_ids.clarify_run_ids.remove(&id).flatten();
                let _ = cmd_tx.try_send(route_command(run_id, AgentCommand::AbortClarify));
            }
            Effect::StopAgent => {
                let _ = cmd_tx.try_send(AgentCommand::Stop {
                    reason: "aborted by user".into(),
                });
            }
            Effect::Quit => {
                return ActionOutcome::Quit;
            }
            Effect::LoadHistory { direction } => {
                load_history_into_textarea(direction, state, textarea);
            }
            Effect::ExitBrowsing => {
                state.exit_input_history_browsing(&textarea.lines().join("\n"));
            }
            Effect::PlaceCursorAtMousePos { column, row } => {
                mouse_drag.place_cursor(state, textarea, column, row);
            }
            Effect::StartInputSelection => {
                mouse_drag.start_selection(textarea);
            }
            Effect::ExtendInputSelection { column, row } => {
                mouse_drag.extend_selection(state, textarea, column, row);
            }
            Effect::FinalizeInputSelection => {
                mouse_drag.finalize_selection(state, textarea, clipboard);
            }
            Effect::CopyToClipboard(text) => {
                if clipboard.write_text(text) {
                    state.set_toast("Copied to clipboard");
                }
            }
            // Slash-command effects need the engine, which this function
            // deliberately does not own; `execute_slash_effects` runs them
            // (they are split out of the batch by `split_slash_effects`
            // before this function ever sees them).
            Effect::ClearConversation
            | Effect::ListSessions
            | Effect::SelectSession { .. }
            | Effect::CancelSessionSelection
            | Effect::Compact
            | Effect::LoadSubagentTranscript { .. }
            | Effect::ListModels { .. }
            | Effect::SwitchModel(_) => {}
        }
    }
    ActionOutcome::Continue
}

/// Split `effects` into the slash-command effects and everything else,
/// preserving each group's original order.
pub(super) fn split_slash_effects(effects: Vec<Effect>) -> (Vec<Effect>, Vec<Effect>) {
    effects.into_iter().partition(|effect| {
        matches!(
            effect,
            Effect::ClearConversation
                | Effect::ListSessions
                | Effect::SelectSession { .. }
                | Effect::CancelSessionSelection
                | Effect::Compact
                | Effect::ListModels { .. }
                | Effect::SwitchModel(_)
        )
    })
}

/// Execute the slash-command effects.
///
/// Split out of [`execute_effects`] because these are the only effects that
/// reach past the textarea/clipboard into the engine — and because nothing
/// here may touch the session store directly: persistence goes through
/// `AgentCommand`s so the agent loop stays the sole `Context` mutator.
pub(super) async fn execute_slash_effects(
    state: &mut AppState,
    effects: Vec<Effect>,
    engine: &Arc<dyn Engine>,
    cmd_tx: &mpsc::Sender<AgentCommand>,
) {
    for effect in effects {
        match effect {
            Effect::Compact => {
                if let Err(error) = cmd_tx.try_send(AgentCommand::Compact) {
                    tracing::warn!(%error, "could not deliver manual compaction command");
                    state.push_notice(format!("Could not request compaction: {error}"));
                }
            }
            Effect::ClearConversation => {
                state.clear_conversation();
                let _ = cmd_tx.send(AgentCommand::ClearContext).await;
                state.push_notice(
                    "Conversation cleared. Following turns are recorded in a new session."
                        .to_string(),
                );
            }
            Effect::ListModels { filter } => {
                state.slash_overlay = SlashOverlay::LoadingModels;
                let result = fetch_model_list(Arc::clone(engine)).await;
                apply_model_list_result(state, filter, result, engine);
            }
            Effect::SwitchModel(selection) => {
                switch_model(state, &selection, engine, cmd_tx).await;
            }
            _ => {}
        }
    }
}
