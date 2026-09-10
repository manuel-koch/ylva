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

//! TUI event loop — Driver layer of the reducer pattern.
//!
//! `run_tui` owns all I/O: it receives [`AppAction`]s from the crossterm
//! bridge, calls `presenter::update` (pure), then executes the returned
//! [`Effect`]s via `execute_effects`. Domain updates from [`AgentLoop`] are
//! converted to [`DomainEvent`]s and routed through the same path.

use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::style::Style;
use ratatui::Terminal;
use ratatui_textarea::{Scrolling, TextArea};
use tokio::select;
use tokio::sync::mpsc;

use crate::agent::tools::DEFAULT_TUI_TOOLS;
use crate::agent::{AgentCommand, AgentLoop, AgentMode, Role, UiUpdate};
use crate::agent_factory::AgentFactory;
use crate::app::App;
use crate::config::ProfileConfig;
use crate::events::{spawn_crossterm_bridge, AppAction};
use crate::llm::engine::{Engine, EngineConfig, EngineImpl};
use crate::llm::Provider;
use crate::presenter::{self, TextareaContext, TuiEvent};
use crate::render;
use crate::tui_reducer_state::{
    engine_choice_label, matching_model_indices, model_choice_label, AppState, ConfirmKey,
    DomainEvent, Effect, Focus, HistoryDirection, InputEvent, ModelChoice, SlashOverlay, TuiMode,
    UserMessageState,
};
use crate::tui_render_state::TuiState;
use crate::ToolRegistry;

/// Abstracts writing text to the OS clipboard so the Driver can be tested
/// without side-effects on the host machine.
pub trait ClipboardSink: Send {
    /// Copy `text` to the clipboard. Returns `true` on success.
    fn write_text(&mut self, text: String) -> bool;
}

/// Production implementation that delegates to [`arboard`].
pub struct ArboardClipboard;

impl ClipboardSink for ArboardClipboard {
    fn write_text(&mut self, text: String) -> bool {
        match arboard::Clipboard::new() {
            Ok(mut clipboard) => clipboard.set_text(text).is_ok(),
            Err(error) => {
                tracing::warn!(?error, "clipboard unavailable");
                false
            }
        }
    }
}

/// No-op clipboard used in tests so that no OS clipboard is touched.
pub struct NoopClipboard;

impl ClipboardSink for NoopClipboard {
    fn write_text(&mut self, _text: String) -> bool {
        true
    }
}

/// Window (seconds) for the key-agnostic double-press confirm on `Ctrl+C`/`Esc`.
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(2);

/// Interval between spinner frame advances while the agent is busy.
const SPINNER_TICK_INTERVAL: Duration = Duration::from_millis(150);

/// Capacity of the bounded action channel between the crossterm bridge and the
/// async loop, and the per-frame `UiUpdate` drain cap.
const CHANNEL_CAPACITY: usize = 64;

/// Outcome of processing one event or effect batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionOutcome {
    Continue,
    Quit,
}

/// Driver-local tracking of which child agent (if any) raised each pending
/// tool/clarify approval. The Presenter/`AppState`/`Effect` surface stays
/// agent-agnostic (an `Effect::ApproveTool { id }` carries no `run_id`); this
/// struct is threaded separately through the Driver alongside `AppState` so
/// the resulting `AgentCommand` can still be routed correctly.
///
/// - `tool_run_ids` maps a pending tool call's `id` (as carried on
///   `UiUpdate::ToolPending`/`Effect::ApproveTool`/`Effect::DenyTool`) to the
///   child's `run_id` that raised it. Populated in `process_one_update` when
///   a `ToolPending` update carries `Some(run_id)`; consumed (removed) in
///   `execute_effects` when the corresponding `ApproveTool`/`DenyTool` effect
///   fires. A missing key means "root's own pending tool" — same meaning as
///   `run_id: None` on the originating `UiUpdate`.
/// - `clarify_run_id` tracks the *current* pending clarify's `run_id`
///   separately, because `Effect::AnswerClarify`/`Effect::AbortClarify`
///   carry no `id` at all (unlike `ApproveTool`/`DenyTool`) — the Presenter
///   already resets `AppState::mode` out of `ClarifyPending` in the same
///   `presenter::update` call that produces these effects, so the id can't
///   be recovered from `state.mode` by the time `execute_effects` runs. Set
///   in `process_one_update` from `UiUpdate::ClarifyPending::run_id`; taken
///   (cleared) in `execute_effects` when `AnswerClarify`/`AbortClarify` fires.
#[derive(Debug, Default)]
struct PendingRunIds {
    tool_run_ids: std::collections::HashMap<uuid::Uuid, uuid::Uuid>,
    clarify_run_id: Option<uuid::Uuid>,
}

/// Wraps `command` in `AgentCommand::ForChild { run_id, .. }` when `run_id`
/// is `Some`; otherwise returns `command` unchanged, exactly as sent today
/// for the root's own pending items.
fn route_command(run_id: Option<uuid::Uuid>, command: AgentCommand) -> AgentCommand {
    match run_id {
        Some(run_id) => AgentCommand::ForChild {
            run_id,
            command: Box::new(command),
        },
        None => command,
    }
}

/// Apply the standard Ylva textarea configuration: word-or-glyph wrapping and
/// no cursor-line underline decoration.
fn configure_textarea(textarea: &mut TextArea<'_>) {
    textarea.set_wrap_mode(ratatui_textarea::WrapMode::WordOrGlyph);
    textarea.set_cursor_line_style(Style::default());
}

/// Map an [`AppAction`] to the typed [`TuiEvent`] the Presenter accepts.
/// Returns `None` for actions that require no presenter call (e.g. `Resize`).
fn app_action_to_tui_event(action: AppAction) -> Option<TuiEvent> {
    match action {
        AppAction::CtrlC => Some(TuiEvent::Input(InputEvent::CtrlC)),
        AppAction::Escape => Some(TuiEvent::Input(InputEvent::Escape)),
        AppAction::ShowHelp => Some(TuiEvent::Input(InputEvent::ShowHelp)),
        AppAction::SigInt => Some(TuiEvent::Input(InputEvent::SigInt)),
        AppAction::KeyInput(event) => Some(TuiEvent::Input(InputEvent::KeyInput(event))),
        AppAction::Paste(text) => Some(TuiEvent::Input(InputEvent::Paste(text))),
        AppAction::MouseInput(mouse) => Some(TuiEvent::Mouse(mouse)),
        AppAction::Resize { .. } => None,
    }
}

/// Map a [`UiUpdate`] to a [`DomainEvent`] for the Presenter.
/// Returns `None` for `ToolError` which is handled directly by the Driver.
fn ui_update_to_domain_event(update: &UiUpdate) -> Option<DomainEvent> {
    match update {
        UiUpdate::StreamStarted => Some(DomainEvent::StreamStarted),
        UiUpdate::AppendText(text) => Some(DomainEvent::StreamChunk(text.clone())),
        UiUpdate::AppendThinking(text) => Some(DomainEvent::StreamThinkingChunk(text.clone())),
        UiUpdate::StreamDone => Some(DomainEvent::StreamDone),
        UiUpdate::StreamCancelled => Some(DomainEvent::StreamCancelled),
        UiUpdate::SteerQueued { run_id } => Some(DomainEvent::SteerQueued { run_id: *run_id }),
        UiUpdate::SteerApplied { run_id } => Some(DomainEvent::SteerApplied { run_id: *run_id }),
        UiUpdate::ToolPending {
            id,
            name,
            arguments,
            content_sections,
            title_arguments,
            run_id,
        } => Some(DomainEvent::ToolPending {
            id: *id,
            name: name.clone(),
            arguments: arguments.clone(),
            content_sections: content_sections.clone(),
            title_arguments: title_arguments.clone(),
            run_id: *run_id,
        }),
        UiUpdate::ClarifyPending {
            id,
            question,
            context,
            choices,
            run_id,
        } => Some(DomainEvent::ClarifyPending {
            id: *id,
            question: question.clone(),
            context: context.clone(),
            choices: choices.clone(),
            run_id: *run_id,
        }),
        UiUpdate::ToolStart {
            id,
            name,
            arguments,
            content_sections,
            title_arguments,
            run_id,
        } => Some(DomainEvent::ToolStart {
            id: *id,
            name: name.clone(),
            arguments: arguments.clone(),
            content_sections: content_sections.clone(),
            title_arguments: title_arguments.clone(),
            run_id: *run_id,
        }),
        UiUpdate::ToolProgress {
            id,
            title_arguments,
        } => Some(DomainEvent::ToolProgress {
            id: *id,
            title_arguments: title_arguments.clone(),
        }),
        UiUpdate::ToolDone {
            id,
            name,
            result_section,
            summary,
            duration,
            content_sections,
            run_id,
        } => Some(DomainEvent::ToolDone {
            id: *id,
            name: name.clone(),
            result_section: result_section.clone(),
            summary: summary.clone(),
            duration: *duration,
            content_sections: content_sections.clone(),
            run_id: *run_id,
        }),
        UiUpdate::ToolKilled { id, run_id } => Some(DomainEvent::ToolKilled {
            id: *id,
            run_id: *run_id,
        }),
        UiUpdate::Error { message, run_id } => Some(DomainEvent::Error {
            message: message.clone(),
            run_id: *run_id,
        }),
        UiUpdate::ChildAgentStarted {
            run_id,
            call_id,
            agent_type,
            task,
            label,
        } => Some(DomainEvent::ChildAgentStarted {
            run_id: *run_id,
            call_id: *call_id,
            agent_type: agent_type.clone(),
            task: task.clone(),
            label: label.clone(),
        }),
        UiUpdate::ChildAgentEngineConfigured { run_id, engine } => {
            Some(DomainEvent::ChildAgentEngineConfigured {
                run_id: *run_id,
                engine: engine.clone(),
            })
        }
        UiUpdate::ChildAgentToken {
            run_id,
            text,
            is_thinking,
        } => Some(DomainEvent::ChildAgentToken {
            run_id: *run_id,
            text: text.clone(),
            is_thinking: *is_thinking,
        }),
        UiUpdate::ChildAgentDone {
            run_id,
            status,
            result,
        } => Some(DomainEvent::ChildAgentDone {
            run_id: *run_id,
            status: status.clone(),
            result: result.clone(),
        }),
        // ToolError and ToolAutoDenied are handled directly by the Driver via
        // AppState; no corresponding DomainEvent exists in the Presenter's surface.
        UiUpdate::ToolError { .. } | UiUpdate::ToolAutoDenied { .. } => None,
    }
}

/// Execute a batch of [`Effect`]s produced by the Presenter, performing the
/// actual I/O (channel sends, textarea mutations, clipboard writes).
/// Returns `ActionOutcome::Quit` if any `Effect::Quit` was processed.
async fn execute_effects(
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
            Effect::SubmitPrompt { .. } => {
                let raw_text = textarea.lines().join("\n");
                let text = dedent(&raw_text);
                let _ = cmd_tx.try_send(AgentCommand::Prompt(text));
            }
            Effect::ClearTextarea => {
                *textarea = TextArea::default();
                configure_textarea(textarea);
                *mouse_drag = MouseDrag::default();
            }
            Effect::SaveTextareaAndClear => {
                let lines = textarea.lines().to_vec();
                let is_empty = lines == vec!["".to_string()];
                state.pending_approval_input_snapshot = if is_empty { None } else { Some(lines) };
                *textarea = TextArea::default();
                configure_textarea(textarea);
            }
            Effect::RestoreTextarea => {
                *textarea = if let Some(snapshot) = state.pending_approval_input_snapshot.take() {
                    TextArea::from(snapshot)
                } else {
                    TextArea::default()
                };
                configure_textarea(textarea);
            }
            Effect::SetInputText(text) => {
                let lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
                *textarea = TextArea::from(lines);
                configure_textarea(textarea);
                textarea.move_cursor(ratatui_textarea::CursorMove::End);
            }
            Effect::TextareaKey(key_event) => {
                textarea.input(key_event);
            }
            Effect::Paste(text) => {
                state.exit_input_history_browsing(&textarea.lines().join("\n"));
                let dedented_text = dedent(&text);
                let paste_lines: Vec<&str> = dedented_text.split('\n').collect();
                for (index, line) in paste_lines.iter().enumerate() {
                    for character in line.chars() {
                        textarea.input(crossterm::event::KeyEvent::new(
                            crossterm::event::KeyCode::Char(character),
                            crossterm::event::KeyModifiers::NONE,
                        ));
                    }
                    if index + 1 < paste_lines.len() {
                        textarea.input(crossterm::event::KeyEvent::new(
                            crossterm::event::KeyCode::Enter,
                            crossterm::event::KeyModifiers::SHIFT,
                        ));
                    }
                }
            }
            Effect::ApproveTool { id } => {
                let run_id = pending_run_ids.tool_run_ids.remove(&id);
                let _ = cmd_tx.try_send(route_command(run_id, AgentCommand::ApproveTool { id }));
            }
            Effect::DenyTool { id, reason } => {
                state.apply_tool_denied(id, &reason);
                let run_id = pending_run_ids.tool_run_ids.remove(&id);
                let _ =
                    cmd_tx.try_send(route_command(run_id, AgentCommand::DenyTool { id, reason }));
            }
            Effect::AnswerClarify(answer) => {
                let run_id = pending_run_ids.clarify_run_id.take();
                let _ = cmd_tx.try_send(route_command(run_id, AgentCommand::AnswerClarify(answer)));
            }
            Effect::AbortClarify => {
                let run_id = pending_run_ids.clarify_run_id.take();
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
                if let Some((cursor_row, cursor_col)) =
                    cursor_for_click(textarea, state.layout.input_area.rect, column, row)
                {
                    state.exit_input_history_browsing(&textarea.lines().join("\n"));
                    textarea.move_cursor(ratatui_textarea::CursorMove::Jump(
                        cursor_row as u16,
                        cursor_col as u16,
                    ));
                    textarea.cancel_selection();
                    *mouse_drag = MouseDrag { moved: false };
                }
            }
            Effect::StartInputSelection => {
                textarea.start_selection();
                mouse_drag.moved = true;
            }
            Effect::ExtendInputSelection { column, row } => {
                if let Some((cursor_row, cursor_col)) =
                    cursor_for_click(textarea, state.layout.input_area.rect, column, row)
                {
                    if !mouse_drag.moved {
                        textarea.start_selection();
                        mouse_drag.moved = true;
                    }
                    textarea.move_cursor(ratatui_textarea::CursorMove::Jump(
                        cursor_row as u16,
                        cursor_col as u16,
                    ));
                }
            }
            Effect::FinalizeInputSelection => {
                if mouse_drag.moved {
                    textarea.copy();
                    let yanked = textarea.yank_text();
                    if !yanked.is_empty() && clipboard.write_text(yanked) {
                        state.set_toast("Copied to clipboard");
                    }
                }
                *mouse_drag = MouseDrag::default();
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
            | Effect::ShowTools
            | Effect::ListModels { .. }
            | Effect::SwitchModel(_) => {}
        }
    }
    ActionOutcome::Continue
}

/// How long `/models` waits for the provider's model list before giving up.
const MODEL_LIST_TIMEOUT: Duration = Duration::from_secs(15);

/// Result of a provider catalog request, including the two failures that need
/// distinct user-facing messages.
enum ModelListResult {
    Models(Vec<crate::llm::ProviderModelOption>),
    ProviderError(anyhow::Error),
    Timeout,
}

/// A model catalog request owned by the TUI driver.
struct PendingModelList {
    filter: Option<String>,
    task: tokio::task::JoinHandle<ModelListResult>,
}

/// Query one provider catalog without tying the request to the driver's event
/// loop.
async fn fetch_model_list(engine: Arc<dyn Engine>) -> ModelListResult {
    match tokio::time::timeout(MODEL_LIST_TIMEOUT, engine.list_model_options()).await {
        Ok(Ok(models)) => ModelListResult::Models(models),
        Ok(Err(error)) => ModelListResult::ProviderError(error),
        Err(_) => ModelListResult::Timeout,
    }
}

/// Start a model catalog request, cancelling any older request whose result is
/// no longer relevant to the visible picker.
fn start_model_list(
    pending_model_list: &mut Option<PendingModelList>,
    filter: Option<String>,
    engine: &Arc<dyn Engine>,
) {
    abort_pending_model_list(pending_model_list);
    let task = tokio::spawn(fetch_model_list(Arc::clone(engine)));
    *pending_model_list = Some(PendingModelList { filter, task });
}

/// Cancel a model catalog request that is no longer needed by the UI.
fn abort_pending_model_list(pending_model_list: &mut Option<PendingModelList>) {
    if let Some(pending) = pending_model_list.take() {
        pending.task.abort();
    }
}

/// Wait for the current catalog request, or remain disabled when no request is
/// active so the main `select!` loop can continue handling other events.
async fn wait_for_model_list(
    pending_model_list: &mut Option<PendingModelList>,
) -> Option<(Option<String>, ModelListResult)> {
    let Some(pending) = pending_model_list.as_mut() else {
        return std::future::pending().await;
    };
    let filter = pending.filter.clone();
    let result = match (&mut pending.task).await {
        Ok(result) => result,
        Err(error) => {
            ModelListResult::ProviderError(anyhow::anyhow!("model catalog task failed: {error}"))
        }
    };
    Some((filter, result))
}

/// Apply a completed model catalog request to the visible picker.
fn apply_model_list_result(
    state: &mut AppState,
    filter: Option<String>,
    result: ModelListResult,
    engine: &Arc<dyn Engine>,
) {
    match result {
        ModelListResult::Models(models) if models.is_empty() => {
            state.slash_overlay = SlashOverlay::Hidden;
            state.push_notice("No configured provider connection reported any models.".to_string());
        }
        ModelListResult::Models(models) => {
            let active_config = engine.config();
            let active_reasoning_effort = engine.reasoning_effort();
            let is_active_option = |provider_connection: &str, model_id: &str| {
                provider_connection == active_config.provider_connection
                    && model_id == active_config.model
            };
            // Looked up so an engine shortcut can show the real context
            // length and advertised reasoning efforts of the model it names,
            // rather than reporting "unavailable" for a model the catalog
            // does list.
            let catalog_by_identity: std::collections::HashMap<
                (String, String),
                crate::llm::ModelInfo,
            > = models
                .iter()
                .map(|option| {
                    (
                        (option.provider_connection.clone(), option.model.id.clone()),
                        option.model.clone(),
                    )
                })
                .collect();
            // Named engines (explicit `[engines.*]` tables, or their
            // implicit one-per-connection equivalent) come first as
            // shortcuts to the provider/model/config they name, ahead of the
            // raw catalog below.
            let engine_choices: Vec<ModelChoice> = engine
                .named_engines()
                .into_iter()
                .map(|(engine_name, engine_config)| {
                    let is_active =
                        is_active_option(&engine_config.provider_connection, &engine_config.model);
                    let catalog_model = catalog_by_identity.get(&(
                        engine_config.provider_connection.clone(),
                        engine_config.model.clone(),
                    ));
                    // The engine's own configured budget wins if TOML set
                    // one; otherwise fall back to the provider's advertised
                    // context length for this model, and finally to the
                    // built-in default.
                    let effective_context_length = engine_config.effective_context_length(
                        catalog_model.and_then(|model| model.context_length),
                    );
                    let model = crate::llm::ModelInfo {
                        id: engine_config.model,
                        context_length: Some(effective_context_length),
                        reasoning_efforts: catalog_model
                            .and_then(|model| model.reasoning_efforts.clone()),
                    };
                    let reasoning_effort = if is_active {
                        active_reasoning_effort.clone()
                    } else {
                        engine_config.reasoning
                    };
                    let selection = crate::llm::ProviderModelOption {
                        provider_connection: engine_config.provider_connection,
                        model,
                        reasoning_effort,
                    };
                    ModelChoice {
                        label: engine_choice_label(&engine_name, &selection, is_active),
                        selection,
                        is_active,
                        engine_name: Some(engine_name),
                    }
                })
                .collect();
            let model_choices: Vec<ModelChoice> = models
                .into_iter()
                .map(|mut option| {
                    let is_active = is_active_option(&option.provider_connection, &option.model.id);
                    if is_active {
                        option.reasoning_effort = active_reasoning_effort.clone();
                    }
                    ModelChoice {
                        label: model_choice_label(&option, is_active),
                        selection: option,
                        is_active,
                        engine_name: None,
                    }
                })
                .collect();
            let choices: Vec<ModelChoice> =
                engine_choices.into_iter().chain(model_choices).collect();
            let filter = filter.unwrap_or_default();
            let selected = matching_model_indices(&choices, &filter)
                .into_iter()
                .position(|index| {
                    choices[index].selection.provider_connection
                        == active_config.provider_connection
                        && choices[index].selection.model.id == active_config.model
                })
                .unwrap_or(0);
            state.slash_overlay = SlashOverlay::ModelPicker {
                choices,
                filter,
                selected,
            };
        }
        ModelListResult::ProviderError(error) => {
            state.slash_overlay = SlashOverlay::Hidden;
            state.push_notice(format!("Could not list models: {error}"));
        }
        ModelListResult::Timeout => {
            state.slash_overlay = SlashOverlay::Hidden;
            state.push_notice(format!(
                "Could not list models: the provider did not answer within {} s.",
                MODEL_LIST_TIMEOUT.as_secs()
            ));
        }
    }
}

/// Split `effects` into the slash-command effects and everything else,
/// preserving each group's original order.
fn split_slash_effects(effects: Vec<Effect>) -> (Vec<Effect>, Vec<Effect>) {
    effects.into_iter().partition(|effect| {
        matches!(
            effect,
            Effect::ClearConversation
                | Effect::ShowTools
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
async fn execute_slash_effects(
    state: &mut AppState,
    effects: Vec<Effect>,
    engine: &Arc<dyn Engine>,
    cmd_tx: &mpsc::Sender<AgentCommand>,
) {
    for effect in effects {
        match effect {
            Effect::ClearConversation => {
                state.clear_conversation();
                let _ = cmd_tx.send(AgentCommand::ClearContext).await;
                state.push_notice(
                    "Conversation cleared. Following turns are recorded in a new session."
                        .to_string(),
                );
            }
            Effect::ShowTools => {
                state.push_notice(tools_notice_text(&state.available_tool_names));
            }
            Effect::ListModels { filter } => {
                state.slash_overlay = SlashOverlay::LoadingModels;
                let result = fetch_model_list(Arc::clone(engine)).await;
                apply_model_list_result(state, filter, result, engine);
            }
            Effect::SwitchModel(selection) => match engine.switch_model_option(&selection) {
                Ok(()) => {
                    let config = engine.config();
                    let confirmation = match selection.model.reasoning_efforts.as_ref() {
                        Some(_) => format!(
                            "Switched to {}/{} (reasoning: {}).",
                            config.provider_connection,
                            config.model,
                            selection.reasoning_effort.as_deref().unwrap_or("default")
                        ),
                        None => format!(
                            "Switched to {}/{}.",
                            config.provider_connection, config.model
                        ),
                    };
                    // TUI-only: never injected into the conversation, since a
                    // mid-conversation `role:"system"` message is rejected by
                    // some providers/chat templates and the switch is already
                    // durably recorded below via `RecordModelChanged`.
                    state.push_notice(confirmation);
                    // Read both values from the live engine because a
                    // selection may have changed the active provider as well
                    // as its model.
                    let _ = cmd_tx
                        .send(AgentCommand::RecordModelChanged {
                            provider_connection: config.provider_connection,
                            model: config.model,
                        })
                        .await;
                }
                Err(error) => {
                    state.push_notice(format!(
                        "Could not switch to {}/{}: {error}",
                        selection.provider_connection, selection.model.id
                    ));
                }
            },
            _ => {}
        }
    }
}

/// Render the `/tools` notice body.
fn tools_notice_text(tool_names: &[String]) -> String {
    if tool_names.is_empty() {
        return "No tools are enabled for this session.".to_string();
    }
    let mut text = String::from("Tools available to the agent:");
    for name in tool_names {
        text.push_str("\n  • ");
        text.push_str(name);
    }
    text
}

/// Process one [`UiUpdate`]: convert to a [`DomainEvent`] (or handle directly
/// for `ToolError`) and run the resulting effects.
#[expect(clippy::too_many_arguments)]
async fn process_one_update(
    state: &mut AppState,
    textarea: &mut TextArea<'_>,
    cmd_tx: &mpsc::Sender<AgentCommand>,
    textarea_ctx: &TextareaContext,
    update: &UiUpdate,
    mouse_drag: &mut MouseDrag,
    clipboard: &mut dyn ClipboardSink,
    pending_run_ids: &mut PendingRunIds,
) -> ActionOutcome {
    if let UiUpdate::ToolError {
        id,
        error,
        duration,
        content_sections,
        run_id,
    } = update
    {
        state.apply_tool_error(
            *id,
            error.clone(),
            *duration,
            content_sections.clone(),
            *run_id,
        );
        return ActionOutcome::Continue;
    }
    if let UiUpdate::ToolAutoDenied {
        id,
        name,
        title_arguments,
        content_sections,
        reason,
        run_id,
    } = update
    {
        state.apply_tool_auto_denied(
            *id,
            name.clone(),
            title_arguments.clone(),
            content_sections.clone(),
            reason.clone(),
            *run_id,
        );
        return ActionOutcome::Continue;
    }
    // Record which child (if any) raised this pending approval *before*
    // converting to the agent-agnostic `DomainEvent`/`Effect` surface — see
    // `PendingRunIds`.
    if let UiUpdate::ToolPending {
        id,
        run_id: Some(run_id),
        ..
    } = update
    {
        pending_run_ids.tool_run_ids.insert(*id, *run_id);
    }
    if let UiUpdate::ClarifyPending { run_id, .. } = update {
        pending_run_ids.clarify_run_id = *run_id;
    }
    if let Some(domain_event) = ui_update_to_domain_event(update) {
        let effects = presenter::update(state, TuiEvent::Domain(domain_event), textarea_ctx);
        execute_effects(
            state,
            textarea,
            cmd_tx,
            effects,
            mouse_drag,
            clipboard,
            pending_run_ids,
        )
        .await
    } else {
        ActionOutcome::Continue
    }
}

/// Convert the driver's `AppState` to the legacy `TuiState` struct for return.
/// Fields not present in `AppState` are given sensible defaults.
fn app_state_to_tui_state(app: AppState) -> TuiState {
    // Extract method results before consuming fields.
    let agent_mode = app.agent_mode();
    let show_help = app.show_help();
    let pending_clarify = if let TuiMode::ClarifyPending {
        question,
        context,
        choices,
        ..
    } = &app.mode
    {
        Some((question.clone(), choices.clone(), context.clone()))
    } else {
        None
    };
    let input_area_rect = app.layout.input_area.rect;
    let chat_area_rect = app.layout.chat_area.rect;
    TuiState {
        agent_mode,
        chat_entries: app.chat_entries,
        partial_assistant_text: app.turn_buffer.partial_assistant_text,
        partial_thinking_text: app.turn_buffer.partial_thinking_text,
        thinking_started_at: app.turn_buffer.thinking_started_at,
        partial_thinking_collapsed: app.turn_buffer.partial_thinking_collapsed,
        engine_display: app.engine_display,
        queued_steer_texts: app.queued_steer_texts,
        steer_indicator: app.steer_indicator,
        pending_approval: None,
        pending_approval_is_subagent: false,
        pending_approval_input_snapshot: app.pending_approval_input_snapshot,
        input_history: app.input_history,
        history_index: app.history_index,
        draft: app.draft,
        input_area: input_area_rect,
        toast: app.toast,
        spinner_frame: app.spinner_frame,
        input_scroll_top: app.input_scroll_top,
        slash_overlay: app.slash_overlay,
        help_popup: crate::tui_render_state::HelpPopupState {
            show_help,
            help_scroll_offset: app.help_popup.help_scroll_offset,
            help_scroll_x: app.help_popup.help_scroll_x,
            help_popup_rect: app.layout.help_popup.rect,
        },
        clarify_banner_state: crate::tui_render_state::ClarifyBannerState {
            pending_clarify,
            clarify_scroll_offset: app.clarify_banner_state.clarify_scroll_offset,
            clarify_banner_drag: None,
            clarify_banner: ratatui::layout::Rect::default(),
        },
        child_view: crate::tui_render_state::ChildViewState {
            child_view_bubble: None,
            header_bubble_scroll_offset: 0,
            header_bubble_area: ratatui::layout::Rect::default(),
        },
        conversation: crate::tui_render_state::ConversationListState {
            scroll_offset: app.conversation.scroll_offset,
            scroll_locked: app.conversation.scroll_locked,
            chat_area_height: app.conversation.chat_area_height,
            chat_area: chat_area_rect,
            chat_rendered_lines: app.conversation.chat_rendered_lines,
            chat_rendered_line_chrome_widths: app.conversation.chat_rendered_line_chrome_widths,
            chat_selection: app.conversation.chat_selection,
            chat_drag_active: app.conversation.chat_drag_active,
            bubble_title_rows: app.conversation.bubble_title_rows,
            thinking_title_rows: app.conversation.thinking_title_rows,
        },
    }
}

/// Draw one frame from the driver's current state.
///
/// Keeps the event-loop render path in one place, including the live engine
/// display and confirmation hint.
fn draw_frame<B>(
    terminal: &mut Terminal<B>,
    state: &mut AppState,
    textarea: &mut TextArea<'_>,
    engine: &Arc<dyn Engine>,
    mouse_pos: Option<(u16, u16)>,
) -> anyhow::Result<()>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    state.engine_display = crate::llm::engine::EngineDisplayInfo::from_engine(engine.as_ref());
    let hint = confirm_hint(state);
    terminal.draw(|frame| {
        render::render(frame, state, textarea, hint.as_deref(), mouse_pos);
    })?;
    Ok(())
}

/// Run the TUI event loop against an existing terminal.
///
/// Uses `AppState` internally (the reducer model) and returns a `TuiState`
/// for backward compatibility with integration tests.
#[doc(hidden)]
pub async fn run_tui<B>(
    terminal: &mut Terminal<B>,
    agent: AgentLoop,
    ui_rx: mpsc::Receiver<UiUpdate>,
    initial_prompt: Option<String>,
    mut action_rx: mpsc::Receiver<AppAction>,
    clipboard: &mut dyn ClipboardSink,
) -> anyhow::Result<TuiState>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let mut agent = agent;
    let mut ui_rx = ui_rx;
    let mut state = AppState::new();
    let mut textarea = TextArea::default();
    configure_textarea(&mut textarea);
    let cmd_tx = agent.command_sender();
    let engine = agent.engine();
    state.engine_display = crate::llm::engine::EngineDisplayInfo::from_engine(engine.as_ref());
    // Sorted once here so `/tools` output is stable — the registry is a
    // `HashMap`, whose iteration order is not.
    let mut available_tool_names = agent.registry().names();
    available_tool_names.sort();
    state.available_tool_names = available_tool_names;

    // Driver-local tracking — not part of AppState.
    let mut mouse_pos: Option<(u16, u16)> = None;
    let mut mouse_drag = MouseDrag::default();
    let mut pending_run_ids = PendingRunIds::default();
    let mut pending_model_list: Option<PendingModelList> = None;

    let handle = agent.run();

    let mut spinner_tick = tokio::time::interval(SPINNER_TICK_INTERVAL);
    spinner_tick.tick().await;

    if let Some(prompt) = initial_prompt {
        if !prompt.is_empty() {
            state.push_user_bubble(prompt.clone(), UserMessageState::Sent);
            state.mode = TuiMode::Pending;
            cmd_tx.send(AgentCommand::Prompt(prompt)).await?;
        }
    }

    loop {
        draw_frame(terminal, &mut state, &mut textarea, &engine, mouse_pos)?;

        let textarea_ctx = TextareaContext {
            is_empty: textarea.lines() == [""],
            text: textarea.lines().join("\n"),
            cursor_row: textarea.cursor().0,
            cursor_row_is_last: textarea.cursor().0 + 1 >= textarea.lines().len().max(1),
        };

        select! {
            model_list_result = wait_for_model_list(&mut pending_model_list) => {
                let Some((filter, result)) = model_list_result else {
                    continue;
                };
                pending_model_list = None;
                if matches!(state.slash_overlay, SlashOverlay::LoadingModels) {
                    apply_model_list_result(&mut state, filter, result, &engine);
                    state.refresh_slash_overlay(&textarea.lines().join("\n"));
                }
            }

            action = action_rx.recv() => {
                let Some(action) = action else { break };

                // Track mouse position for hover highlight in the footer.
                if let AppAction::MouseInput(mouse) = &action {
                    mouse_pos = Some((mouse.column, mouse.row));
                }

                // Input-area scroll wheel is not handled by the Presenter
                // (it returns vec![] for that case); the Driver applies it
                // directly to the textarea and syncs input_scroll_top.
                if let AppAction::MouseInput(mouse) = action {
                    let mouse_kind = mouse.kind;
                    let mouse_col = mouse.column;
                    let mouse_row = mouse.row;

                    if matches!(
                        mouse_kind,
                        crossterm::event::MouseEventKind::ScrollUp
                            | crossterm::event::MouseEventKind::ScrollDown
                    ) && state.layout.input_area.contains(mouse_col, mouse_row)
                        && !matches!(state.focus, Focus::HelpPopup)
                    {
                        let delta =
                            if matches!(mouse_kind, crossterm::event::MouseEventKind::ScrollUp) {
                                -1i16
                            } else {
                                1i16
                            };
                        textarea.scroll(Scrolling::Delta { rows: delta, cols: 0 });
                        if delta < 0 {
                            state.input_scroll_top = state.input_scroll_top.saturating_sub(1);
                        } else {
                            state.input_scroll_top = state.input_scroll_top.saturating_add(1);
                        }
                        continue;
                    }

                    let effects =
                        presenter::update(&mut state, TuiEvent::Mouse(mouse), &textarea_ctx);
                    if execute_effects(
                        &mut state, &mut textarea, &cmd_tx, effects, &mut mouse_drag, clipboard,
                        &mut pending_run_ids,
                    )
                    .await
                        == ActionOutcome::Quit
                    {
                        break;
                    }
                    continue;
                }

                let Some(event) = app_action_to_tui_event(action) else { continue };
                let was_loading_models = matches!(state.slash_overlay, SlashOverlay::LoadingModels);
                let effects = presenter::update(&mut state, event, &textarea_ctx);
                // Slash-command effects are the only ones needing the engine,
                // and only key input can produce them.
                let (slash_effects, effects) = split_slash_effects(effects);
                if execute_effects(
                    &mut state, &mut textarea, &cmd_tx, effects, &mut mouse_drag, clipboard,
                    &mut pending_run_ids,
                )
                .await
                    == ActionOutcome::Quit
                {
                    break;
                }
                let (model_list_effects, immediate_slash_effects): (Vec<_>, Vec<_>) =
                    slash_effects.into_iter().partition(|effect| {
                        matches!(effect, Effect::ListModels { .. })
                    });
                let mut started_model_list = false;
                for effect in model_list_effects {
                    if let Effect::ListModels { filter } = effect {
                        state.slash_overlay = SlashOverlay::LoadingModels;
                        start_model_list(&mut pending_model_list, filter, &engine);
                        started_model_list = true;
                    }
                }
                execute_slash_effects(
                    &mut state,
                    immediate_slash_effects,
                    &engine,
                    &cmd_tx,
                )
                .await;
                if was_loading_models
                    && !started_model_list
                    && !matches!(state.slash_overlay, SlashOverlay::LoadingModels)
                {
                    abort_pending_model_list(&mut pending_model_list);
                }
                // The suggestion overlay tracks the draft, so it is
                // recomputed after every key that may have changed it.
                state.refresh_slash_overlay(&textarea.lines().join("\n"));
            }

            update = ui_rx.recv() => {
                let Some(update) = update else { break };

                if process_one_update(
                    &mut state, &mut textarea, &cmd_tx, &textarea_ctx, &update, &mut mouse_drag,
                    clipboard, &mut pending_run_ids,
                )
                .await
                    == ActionOutcome::Quit
                {
                    break;
                }

                let mut drain_quit = false;
                for _ in 0..CHANNEL_CAPACITY {
                    match ui_rx.try_recv() {
                        Ok(next) => {
                            if process_one_update(
                                &mut state, &mut textarea, &cmd_tx, &textarea_ctx,
                                &next, &mut mouse_drag, clipboard, &mut pending_run_ids,
                            )
                            .await
                                == ActionOutcome::Quit
                            {
                                drain_quit = true;
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                if drain_quit {
                    break;
                }
            }

            _ = confirm_timeout(&state) => {
                let effects = presenter::update(
                    &mut state,
                    TuiEvent::Domain(DomainEvent::ConfirmExpired),
                    &textarea_ctx,
                );
                execute_effects(
                    &mut state, &mut textarea, &cmd_tx, effects, &mut mouse_drag, clipboard,
                    &mut pending_run_ids,
                )
                .await;
            }

            _ = bubble_click_timeout(&state) => {
                let effects = presenter::update(
                    &mut state,
                    TuiEvent::Domain(DomainEvent::BubbleClickWindowExpired),
                    &textarea_ctx,
                );
                execute_effects(
                    &mut state, &mut textarea, &cmd_tx, effects, &mut mouse_drag, clipboard,
                    &mut pending_run_ids,
                )
                .await;
            }

            _ = spinner_tick.tick() => {
                if spinner_should_tick(&state) {
                    state.spinner_frame = state.spinner_frame.wrapping_add(1);
                }
            }
        }
    }

    abort_pending_model_list(&mut pending_model_list);
    drop(cmd_tx);
    drop(agent);
    handle.await??;
    Ok(app_state_to_tui_state(state))
}

/// Tracks a mouse press inside the input box so drag events can start a
/// text selection on first movement.
#[derive(Debug, Clone, Copy, Default)]
struct MouseDrag {
    moved: bool,
}

/// Dedent `text`: remove the common leading whitespace from all non-empty
/// lines (via `textwrap::dedent`), then strip leading/trailing blank-only lines.
fn dedent(text: &str) -> String {
    let dedented = textwrap::dedent(text);
    let lines: Vec<&str> = dedented.lines().collect();
    let Some(start) = lines.iter().position(|l| !l.trim().is_empty()) else {
        return String::new();
    };
    let end = lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map(|i| i + 1)
        .unwrap_or(lines.len());
    lines[start..end].join("\n")
}

/// Translate a mouse `(column, row)` inside the input box `area` into a
/// (line, column) cursor position in `textarea`. Returns `None` when the point
/// is outside the input box (e.g. on its border).
fn cursor_for_click(
    textarea: &TextArea<'_>,
    area: ratatui::layout::Rect,
    column: u16,
    row: u16,
) -> Option<(usize, usize)> {
    if column < area.x.saturating_add(1) || row < area.y {
        return None;
    }
    let width = area.width.saturating_sub(1).max(1) as usize;
    let screen_row = (row - area.y) as usize;
    let screen_col = (column - area.x - 1) as usize;
    let lines = textarea.lines();
    let mut row_start = 0usize;
    for (line_index, line) in lines.iter().enumerate() {
        let segments = textwrap::wrap(line, width);
        let row_count = segments.len().max(1);
        let row_end = row_start + row_count;
        if screen_row < row_end {
            let segment = screen_row.saturating_sub(row_start).min(row_count - 1);
            let mut search_from = 0usize;
            let mut start_col = 0usize;
            for (index, part) in segments.iter().take(segment + 1).enumerate() {
                let offset = line[search_from..].find(part.as_ref()).unwrap_or(0) + search_from;
                start_col += line[search_from..offset].chars().count();
                if index == segment {
                    break;
                }
                start_col += part.chars().count();
                search_from = offset + part.len();
            }
            let col = (start_col + screen_col).min(line.chars().count());
            return Some((line_index, col));
        }
        row_start = row_end;
    }
    let last = lines.len().saturating_sub(1);
    Some((last, lines.get(last).map_or(0, |line| line.chars().count())))
}

/// Loads a history entry into the textarea. Returns `true` when a history
/// entry was loaded.
fn load_history_into_textarea(
    direction: HistoryDirection,
    state: &mut AppState,
    textarea: &mut TextArea<'_>,
) -> bool {
    let current = textarea.lines().join("\n");
    if let Some(loaded) = state.navigate_input_message_history(direction, &current) {
        let lines: Vec<String> = loaded.split('\n').map(str::to_owned).collect();
        *textarea = TextArea::from(lines);
        configure_textarea(textarea);
        textarea.move_cursor(ratatui_textarea::CursorMove::End);
        true
    } else {
        false
    }
}

/// Returns the footer hint string when a double-press confirm is pending.
fn confirm_hint(state: &AppState) -> Option<String> {
    if matches!(state.mode, TuiMode::Aborting) {
        return None;
    }
    if let TuiMode::DoublePressAbort {
        return_to,
        armed_by,
        ..
    } = &state.mode
    {
        let is_idle = matches!(**return_to, TuiMode::Idle);
        // Only the same key's second press confirms (see `ConfirmKey`), so
        // name that one key explicitly rather than both — naming both would
        // wrongly imply either key finishes the same action.
        let hint = match (armed_by, is_idle) {
            (ConfirmKey::CtrlC, true) => " Press ctrl-c again to quit",
            (ConfirmKey::CtrlC, false) => " Press ctrl-c again to cancel & quit",
            (ConfirmKey::Escape, true) => " Press esc again to clear",
            (ConfirmKey::Escape, false) => " Press esc again to cancel",
        };
        Some(hint.to_string())
    } else {
        None
    }
}

/// Whether the footer/header spinner glyph should advance this tick.
///
/// Gates on `agent_mode()`, not raw `TuiMode`, so the spinner keeps advancing
/// whenever *anything* is animating a spinner glyph — including a
/// `spawn_subagent` tool bubble (and its child-view header re-render) that's
/// still `Running` while the root's own `TuiMode` has already gone `Idle`
/// waiting on it, regardless of whether the subagent itself is currently
/// paused for its own tool approval (that only affects the subagent's
/// *inner* bubble, not the root `spawn_subagent` bubble's `Running` state).
/// `DoublePressAbort` stays frozen regardless, matching its static
/// confirm-prompt UI.
fn spinner_should_tick(state: &AppState) -> bool {
    !matches!(state.mode, TuiMode::DoublePressAbort { .. }) && state.agent_mode() != AgentMode::Idle
}

/// A future that resolves once the double-press confirm window elapses (or
/// never, when no confirm is pending).
async fn confirm_timeout(state: &AppState) {
    if let TuiMode::DoublePressAbort { pressed_at, .. } = &state.mode {
        let remaining = (*pressed_at + CONFIRM_TIMEOUT).saturating_duration_since(Instant::now());
        tokio::time::sleep(remaining).await;
    } else {
        std::future::pending::<()>().await;
    }
}

/// A future that resolves once a bubble's deferred single-click
/// collapse/expand toggle window elapses (or never, when no click is
/// currently armed). Armed by a first click on a `spawn_subagent` bubble's
/// title — see `crate::tui_reducer_state::DOUBLE_CLICK_WINDOW` for why the
/// toggle is deferred rather than applied immediately.
async fn bubble_click_timeout(state: &AppState) {
    if let Some((_, clicked_at)) = state.last_bubble_click {
        let remaining = (clicked_at + crate::tui_reducer_state::DOUBLE_CLICK_WINDOW)
            .saturating_duration_since(Instant::now());
        tokio::time::sleep(remaining).await;
    } else {
        std::future::pending::<()>().await;
    }
}

/// Top-level TUI application entry point.
///
/// Builds a real `CrosstermBackend` terminal, enables raw mode + alternate
/// screen, installs the SIGINT handler, constructs an [`AgentLoop`] via
/// `AgentFactory`, and guarantees the terminal is restored on every exit path.
pub struct TuiApp {
    factory: Arc<AgentFactory>,
    profile: ProfileConfig,
    agent_type: String,
    explicit_prompt: Option<String>,
}

impl TuiApp {
    pub fn new(
        factory: Arc<AgentFactory>,
        profile: ProfileConfig,
        agent_type: String,
        explicit_prompt: Option<String>,
    ) -> Self {
        TuiApp {
            factory,
            profile,
            agent_type,
            explicit_prompt,
        }
    }
}

impl App for TuiApp {
    async fn run(self) -> anyhow::Result<u8> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(
            stdout,
            EnterAlternateScreen,
            Hide,
            EnableMouseCapture,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
            EnableBracketedPaste,
        )?;
        let _guard = TerminalGuard;

        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let (action_tx, action_rx) = mpsc::channel::<AppAction>(CHANNEL_CAPACITY);
        spawn_crossterm_bridge(action_tx.clone());
        install_sigint_relay(action_tx)?;

        let (agent, ui_rx) = self
            .factory
            .create(&self.profile, &self.agent_type, Role::Root)?;

        run_tui(
            &mut terminal,
            agent,
            ui_rx,
            self.explicit_prompt,
            action_rx,
            &mut ArboardClipboard,
        )
        .await
        .map(|_| 0u8)
    }
}

/// Compatibility shim for `main.rs` until Phase 5 Step 9 migrates the binary
/// to construct `TuiApp` directly.
///
/// TODO Phase 5 Step 9: remove this function.
pub async fn run_tui_on_stdout(
    provider: Arc<dyn Provider>,
    registry: Arc<ToolRegistry>,
    initial_prompt: Option<String>,
) -> anyhow::Result<()> {
    let engine_config = EngineConfig {
        provider_connection: provider.name().to_string(),
        model: String::new(),
        temperature: None,
        context_length: None,
        api_base_url: None,
        api_key: None,
        reasoning: None,
    };
    let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(provider, engine_config));
    let factory = Arc::new(
        AgentFactory::new(std::collections::HashMap::from([(
            "default".to_string(),
            engine,
        )]))
        .with_allowed_tools(DEFAULT_TUI_TOOLS),
    );
    let profile = ProfileConfig::default();
    let _ = registry; // registry is created internally by AgentFactory; parameter kept for API compat
    TuiApp::new(factory, profile, "default".to_string(), initial_prompt)
        .run()
        .await
        .map(|_| ())
}

/// RAII guard restoring the terminal to its pre-TUI state.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            Show,
            DisableMouseCapture,
            PopKeyboardEnhancementFlags,
            DisableBracketedPaste,
        );
        let _ = disable_raw_mode();
    }
}

/// Bridge OS SIGINT to `AppAction::SigInt`.
fn install_sigint_relay(action_tx: mpsc::Sender<AppAction>) -> anyhow::Result<()> {
    let (sigint_tx, sigint_rx) = std::sync::mpsc::channel::<()>();
    ctrlc::set_handler(move || {
        let _ = sigint_tx.send(());
    })?;
    tokio::task::spawn_blocking(move || {
        while !action_tx.is_closed() {
            match sigint_rx.recv_timeout(Duration::from_millis(50)) {
                Ok(()) => {
                    if action_tx.blocking_send(AppAction::SigInt).is_err() {
                        break;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::engine::DEFAULT_CONTEXT_LENGTH;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tokio::sync::mpsc;
    use uuid::Uuid;

    fn key_event(code: KeyCode) -> crossterm::event::KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn textarea_ctx_from(textarea: &TextArea<'_>) -> TextareaContext {
        TextareaContext {
            is_empty: textarea.lines() == [""],
            text: textarea.lines().join("\n"),
            cursor_row: textarea.cursor().0,
            cursor_row_is_last: textarea.cursor().0 + 1 >= textarea.lines().len().max(1),
        }
    }

    fn setup() -> (
        AppState,
        TextArea<'static>,
        mpsc::Sender<AgentCommand>,
        mpsc::Receiver<AgentCommand>,
    ) {
        let (cmd_tx, cmd_rx) = mpsc::channel(16);
        let mut textarea = TextArea::default();
        configure_textarea(&mut textarea);
        (AppState::new(), textarea, cmd_tx, cmd_rx)
    }

    #[test]
    fn spinner_ticks_while_root_is_idle_but_a_spawned_child_is_still_running() {
        // Regression: the root loop's own `TuiMode` can settle to `Idle` while
        // a spawned subagent's `spawn_subagent` tool bubble stays `Running` —
        // the spinner must keep advancing (driving both the footer and the
        // child-view header bubble's spinner glyph) rather than freezing.
        let mut state = AppState::new();
        state.mode = TuiMode::Idle;
        state
            .chat_entries
            .push(crate::tui_reducer_state::ChatEntry::ToolBubble {
                id: Uuid::new_v4(),
                name: "spawn_subagent".to_string(),
                state: Box::new(crate::tui_reducer_state::ToolCallState::Running {
                    arguments: serde_json::json!({}),
                    content_sections: vec![],
                    title_arguments: vec![],
                    progress_arguments: vec![],
                }),
                collapsed: true,
            });
        assert!(spinner_should_tick(&state));
    }

    #[test]
    fn spinner_keeps_ticking_while_the_spawned_child_itself_is_paused_for_approval() {
        // The child's own inner tool-approval pause sets `Focus::ToolApproval`
        // but does not touch the root `spawn_subagent` bubble's `Running`
        // state or the root's `TuiMode` — the spinner must not stall just
        // because *something* is paused.
        let mut state = AppState::new();
        state.mode = TuiMode::Idle;
        state
            .chat_entries
            .push(crate::tui_reducer_state::ChatEntry::ToolBubble {
                id: Uuid::new_v4(),
                name: "spawn_subagent".to_string(),
                state: Box::new(crate::tui_reducer_state::ToolCallState::Running {
                    arguments: serde_json::json!({}),
                    content_sections: vec![],
                    title_arguments: vec![],
                    progress_arguments: vec![],
                }),
                collapsed: true,
            });
        state.focus = crate::tui_reducer_state::Focus::ToolApproval {
            tool_id: Uuid::new_v4(),
            tool_name: "bash".to_string(),
        };
        assert!(spinner_should_tick(&state));
    }

    #[test]
    fn spinner_does_not_tick_while_truly_idle() {
        let state = AppState::new();
        assert!(!spinner_should_tick(&state));
    }

    #[test]
    fn spinner_does_not_tick_during_double_press_abort() {
        let mut state = AppState::new();
        state.mode = TuiMode::DoublePressAbort {
            pressed_at: Instant::now(),
            return_to: Box::new(TuiMode::Streaming),
            armed_by: ConfirmKey::CtrlC,
        };
        assert!(!spinner_should_tick(&state));
    }

    #[test]
    fn idle_ctrl_c_first_press_enters_double_press_mode() {
        let (mut state, textarea, _cmd_tx, _cmd_rx) = setup();
        let ctx = textarea_ctx_from(&textarea);
        let effects = presenter::update(&mut state, TuiEvent::Input(InputEvent::CtrlC), &ctx);
        assert!(
            effects.is_empty(),
            "first press: no effects (hint shown via mode)"
        );
        assert!(
            matches!(state.mode, TuiMode::DoublePressAbort { .. }),
            "mode should be DoublePressAbort after first press"
        );
    }

    #[tokio::test]
    async fn idle_ctrl_c_double_press_quits() {
        let (mut state, mut textarea, cmd_tx, _cmd_rx) = setup();
        let ctx = textarea_ctx_from(&textarea);
        // First press
        presenter::update(&mut state, TuiEvent::Input(InputEvent::CtrlC), &ctx);
        // Second press
        let effects = presenter::update(&mut state, TuiEvent::Input(InputEvent::CtrlC), &ctx);
        assert!(
            effects.contains(&Effect::Quit),
            "second press should emit Quit"
        );
        let outcome = execute_effects(
            &mut state,
            &mut textarea,
            &cmd_tx,
            effects,
            &mut MouseDrag::default(),
            &mut NoopClipboard,
            &mut PendingRunIds::default(),
        )
        .await;
        assert_eq!(outcome, ActionOutcome::Quit);
    }

    #[tokio::test]
    async fn idle_escape_clears_input_without_quit() {
        let (mut state, _, cmd_tx, _cmd_rx) = setup();
        let mut textarea = TextArea::from(vec!["draft".to_string()]);
        configure_textarea(&mut textarea);
        let ctx = textarea_ctx_from(&textarea);
        let effects = presenter::update(&mut state, TuiEvent::Input(InputEvent::Escape), &ctx);
        assert!(effects.contains(&Effect::ClearTextarea));
        assert!(!effects.contains(&Effect::Quit));
        execute_effects(
            &mut state,
            &mut textarea,
            &cmd_tx,
            effects,
            &mut MouseDrag::default(),
            &mut NoopClipboard,
            &mut PendingRunIds::default(),
        )
        .await;
        assert_eq!(textarea.lines(), &[""]);
    }

    #[tokio::test]
    async fn busy_escape_with_draft_clears_input_without_stopping() {
        let (mut state, _, cmd_tx, mut cmd_rx) = setup();
        state.mode = TuiMode::Streaming;
        let mut textarea = TextArea::from(vec!["my draft".to_string()]);
        configure_textarea(&mut textarea);
        let ctx = textarea_ctx_from(&textarea);
        let effects = presenter::update(&mut state, TuiEvent::Input(InputEvent::Escape), &ctx);
        assert!(effects.contains(&Effect::ClearTextarea));
        execute_effects(
            &mut state,
            &mut textarea,
            &cmd_tx,
            effects,
            &mut MouseDrag::default(),
            &mut NoopClipboard,
            &mut PendingRunIds::default(),
        )
        .await;
        assert_eq!(textarea.lines(), &[""]);
        assert!(cmd_rx.try_recv().is_err(), "no Stop should be sent");
    }

    #[tokio::test]
    async fn busy_escape_with_empty_input_triggers_confirm_to_stop() {
        let (mut state, mut textarea, cmd_tx, mut cmd_rx) = setup();
        state.mode = TuiMode::Streaming;
        let ctx = textarea_ctx_from(&textarea);
        // First Esc: enters double-press mode.
        let effects = presenter::update(&mut state, TuiEvent::Input(InputEvent::Escape), &ctx);
        assert!(effects.is_empty());
        assert!(matches!(state.mode, TuiMode::DoublePressAbort { .. }));
        assert!(cmd_rx.try_recv().is_err());
        // Second Esc: sends Stop.
        let effects2 = presenter::update(&mut state, TuiEvent::Input(InputEvent::Escape), &ctx);
        assert!(effects2.contains(&Effect::StopAgent));
        execute_effects(
            &mut state,
            &mut textarea,
            &cmd_tx,
            effects2,
            &mut MouseDrag::default(),
            &mut NoopClipboard,
            &mut PendingRunIds::default(),
        )
        .await;
        assert!(matches!(cmd_rx.try_recv(), Ok(AgentCommand::Stop { .. })));
    }

    #[tokio::test]
    async fn busy_double_press_ctrl_c_sends_stop() {
        let (mut state, mut textarea, cmd_tx, mut cmd_rx) = setup();
        state.mode = TuiMode::Streaming;
        let ctx = textarea_ctx_from(&textarea);
        presenter::update(&mut state, TuiEvent::Input(InputEvent::CtrlC), &ctx);
        let effects = presenter::update(&mut state, TuiEvent::Input(InputEvent::CtrlC), &ctx);
        assert!(effects.contains(&Effect::StopAgent));
        execute_effects(
            &mut state,
            &mut textarea,
            &cmd_tx,
            effects,
            &mut MouseDrag::default(),
            &mut NoopClipboard,
            &mut PendingRunIds::default(),
        )
        .await;
        assert!(matches!(cmd_rx.try_recv(), Ok(AgentCommand::Stop { .. })));
        assert!(
            matches!(state.mode, TuiMode::Aborting),
            "mode should be Aborting after stop"
        );
    }

    #[tokio::test]
    async fn busy_ctrl_c_with_draft_clears_input_without_stopping() {
        let (mut state, _, cmd_tx, mut cmd_rx) = setup();
        state.mode = TuiMode::Streaming;
        let mut textarea = TextArea::from(vec!["my draft".to_string()]);
        configure_textarea(&mut textarea);
        let ctx = textarea_ctx_from(&textarea);
        let effects = presenter::update(&mut state, TuiEvent::Input(InputEvent::CtrlC), &ctx);
        assert!(effects.contains(&Effect::ClearTextarea));
        execute_effects(
            &mut state,
            &mut textarea,
            &cmd_tx,
            effects,
            &mut MouseDrag::default(),
            &mut NoopClipboard,
            &mut PendingRunIds::default(),
        )
        .await;
        assert_eq!(textarea.lines(), &[""]);
        assert!(cmd_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn tool_pending_ctrl_c_denies_tool_immediately() {
        let (mut state, mut textarea, cmd_tx, mut cmd_rx) = setup();
        let tool_id = uuid::Uuid::new_v4();
        // Simulate ToolPending arriving via domain event.
        let effects = presenter::update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolPending {
                id: tool_id,
                name: "read".to_string(),
                arguments: serde_json::json!({}),
                content_sections: vec![],

                title_arguments: vec![],
                run_id: None,
            }),
            &textarea_ctx_from(&textarea),
        );
        execute_effects(
            &mut state,
            &mut textarea,
            &cmd_tx,
            effects,
            &mut MouseDrag::default(),
            &mut NoopClipboard,
            &mut PendingRunIds::default(),
        )
        .await;
        assert!(matches!(state.focus, Focus::ToolApproval { .. }));

        let ctx = textarea_ctx_from(&textarea);
        let effects = presenter::update(&mut state, TuiEvent::Input(InputEvent::CtrlC), &ctx);
        assert!(effects.contains(&Effect::DenyTool {
            id: tool_id,
            reason: String::new(),
        }));
        execute_effects(
            &mut state,
            &mut textarea,
            &cmd_tx,
            effects,
            &mut MouseDrag::default(),
            &mut NoopClipboard,
            &mut PendingRunIds::default(),
        )
        .await;
        assert_eq!(
            cmd_rx.try_recv(),
            Ok(AgentCommand::DenyTool {
                id: tool_id,
                reason: String::new(),
            })
        );
    }

    #[tokio::test]
    async fn approval_enter_sends_approve_tool() {
        let (mut state, mut textarea, cmd_tx, mut cmd_rx) = setup();
        let tool_id = uuid::Uuid::new_v4();
        let setup_effects = presenter::update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolPending {
                id: tool_id,
                name: "read".to_string(),
                arguments: serde_json::json!({}),
                content_sections: vec![],

                title_arguments: vec![],
                run_id: None,
            }),
            &textarea_ctx_from(&textarea),
        );
        execute_effects(
            &mut state,
            &mut textarea,
            &cmd_tx,
            setup_effects,
            &mut MouseDrag::default(),
            &mut NoopClipboard,
            &mut PendingRunIds::default(),
        )
        .await;

        let ctx = textarea_ctx_from(&textarea);
        let effects = presenter::update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Enter))),
            &ctx,
        );
        assert!(effects.contains(&Effect::ApproveTool { id: tool_id }));
        execute_effects(
            &mut state,
            &mut textarea,
            &cmd_tx,
            effects,
            &mut MouseDrag::default(),
            &mut NoopClipboard,
            &mut PendingRunIds::default(),
        )
        .await;
        assert!(matches!(state.focus, Focus::MainInput));
        assert_eq!(
            cmd_rx.try_recv(),
            Ok(AgentCommand::ApproveTool { id: tool_id })
        );
    }

    #[tokio::test]
    async fn approval_escape_sends_deny_tool() {
        let (mut state, mut textarea, cmd_tx, mut cmd_rx) = setup();
        let tool_id = uuid::Uuid::new_v4();
        let setup_effects = presenter::update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolPending {
                id: tool_id,
                name: "read".to_string(),
                arguments: serde_json::json!({}),
                content_sections: vec![],

                title_arguments: vec![],
                run_id: None,
            }),
            &textarea_ctx_from(&textarea),
        );
        execute_effects(
            &mut state,
            &mut textarea,
            &cmd_tx,
            setup_effects,
            &mut MouseDrag::default(),
            &mut NoopClipboard,
            &mut PendingRunIds::default(),
        )
        .await;

        let ctx = textarea_ctx_from(&textarea);
        let effects = presenter::update(&mut state, TuiEvent::Input(InputEvent::Escape), &ctx);
        assert!(effects.contains(&Effect::DenyTool {
            id: tool_id,
            reason: String::new(),
        }));
        execute_effects(
            &mut state,
            &mut textarea,
            &cmd_tx,
            effects,
            &mut MouseDrag::default(),
            &mut NoopClipboard,
            &mut PendingRunIds::default(),
        )
        .await;
        assert!(matches!(state.focus, Focus::MainInput));
        assert_eq!(
            cmd_rx.try_recv(),
            Ok(AgentCommand::DenyTool {
                id: tool_id,
                reason: String::new(),
            })
        );
    }

    #[tokio::test]
    async fn save_and_restore_textarea_for_approval() {
        let (mut state, _, cmd_tx, _cmd_rx) = setup();
        let mut textarea = TextArea::from(vec!["hello world".to_string()]);
        configure_textarea(&mut textarea);
        let setup_effects = presenter::update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolPending {
                id: uuid::Uuid::new_v4(),
                name: "read".to_string(),
                arguments: serde_json::json!({}),
                content_sections: vec![],

                title_arguments: vec![],
                run_id: None,
            }),
            &textarea_ctx_from(&textarea),
        );
        execute_effects(
            &mut state,
            &mut textarea,
            &cmd_tx,
            setup_effects,
            &mut MouseDrag::default(),
            &mut NoopClipboard,
            &mut PendingRunIds::default(),
        )
        .await;
        assert_eq!(
            state.pending_approval_input_snapshot,
            Some(vec!["hello world".to_string()])
        );
        assert_eq!(textarea.lines(), &[""]);

        // Restore
        execute_effects(
            &mut state,
            &mut textarea,
            &cmd_tx,
            vec![Effect::RestoreTextarea],
            &mut MouseDrag::default(),
            &mut NoopClipboard,
            &mut PendingRunIds::default(),
        )
        .await;
        assert_eq!(textarea.lines(), &["hello world"]);
        assert_eq!(state.pending_approval_input_snapshot, None);
    }

    #[test]
    fn help_toggle_via_show_help_event() {
        let (mut state, textarea, _cmd_tx, _cmd_rx) = setup();
        let ctx = textarea_ctx_from(&textarea);
        presenter::update(&mut state, TuiEvent::Input(InputEvent::ShowHelp), &ctx);
        assert!(matches!(state.focus, Focus::HelpPopup));
        presenter::update(&mut state, TuiEvent::Input(InputEvent::ShowHelp), &ctx);
        assert!(matches!(state.focus, Focus::MainInput));
    }

    #[test]
    fn escape_closes_help_popup_before_denying_pending_approval() {
        let (mut state, textarea, _cmd_tx, _cmd_rx) = setup();
        let tool_id = uuid::Uuid::new_v4();
        state.focus = Focus::HelpPopup;
        // Simulate tool still pending — put focus back after this test would
        // normally set it via ToolPending.
        let ctx = textarea_ctx_from(&textarea);
        let effects = presenter::update(&mut state, TuiEvent::Input(InputEvent::Escape), &ctx);
        // Escape from HelpPopup should close it without emitting DenyTool.
        assert!(matches!(state.focus, Focus::MainInput));
        assert!(
            !effects.iter().any(|e| matches!(e, Effect::DenyTool { .. })),
            "no DenyTool should be emitted when closing help popup"
        );
        let _ = tool_id; // used in setup
    }

    // ── slash commands (Phase 7, blueprint §6) ──────────────────────────────

    /// Engine double whose model list and switch outcome are scripted, so the
    /// `/models` flow can be exercised without a provider.
    struct StubEngine {
        models: Vec<crate::llm::ModelInfo>,
        switched_to: std::sync::Mutex<Vec<String>>,
        config: std::sync::Mutex<EngineConfig>,
        named_engines: Vec<(String, EngineConfig)>,
    }

    impl StubEngine {
        fn with_models(model_ids: &[&str], active_model: &str) -> Arc<StubEngine> {
            Arc::new(StubEngine {
                models: model_ids
                    .iter()
                    .map(|id| crate::llm::ModelInfo {
                        id: (*id).to_string(),
                        context_length: None,
                        reasoning_efforts: None,
                    })
                    .collect(),
                switched_to: std::sync::Mutex::new(Vec::new()),
                config: std::sync::Mutex::new(EngineConfig {
                    provider_connection: "stub".to_string(),
                    model: active_model.to_string(),
                    temperature: None,
                    context_length: None,
                    api_base_url: None,
                    api_key: None,
                    reasoning: None,
                }),
                named_engines: Vec::new(),
            })
        }

        /// Like [`Self::with_models`], but also configures one named-engine
        /// shortcut whose model advertises `reasoning_efforts` and is seeded
        /// with `engine_reasoning` — reproducing a profile's
        /// `[engines.<name>]` table for `/models` shortcut tests. The
        /// catalog's advertised `context_length` (`catalog_context_length`)
        /// is set to a different value than `engine_context_length` so
        /// tests can tell which one the shortcut actually displays.
        #[allow(clippy::too_many_arguments)]
        fn with_named_engine(
            model_ids: &[&str],
            active_model: &str,
            engine_name: &str,
            engine_model: &str,
            reasoning_efforts: &[&str],
            engine_reasoning: Option<&str>,
            engine_context_length: Option<usize>,
            catalog_context_length: Option<usize>,
        ) -> Arc<StubEngine> {
            Arc::new(StubEngine {
                models: model_ids
                    .iter()
                    .map(|id| crate::llm::ModelInfo {
                        id: (*id).to_string(),
                        context_length: if *id == engine_model {
                            catalog_context_length
                        } else {
                            None
                        },
                        reasoning_efforts: if *id == engine_model {
                            Some(
                                reasoning_efforts
                                    .iter()
                                    .map(|effort| effort.to_string())
                                    .collect(),
                            )
                        } else {
                            None
                        },
                    })
                    .collect(),
                switched_to: std::sync::Mutex::new(Vec::new()),
                config: std::sync::Mutex::new(EngineConfig {
                    provider_connection: "stub".to_string(),
                    model: active_model.to_string(),
                    temperature: None,
                    context_length: None,
                    api_base_url: None,
                    api_key: None,
                    reasoning: None,
                }),
                named_engines: vec![(
                    engine_name.to_string(),
                    EngineConfig {
                        provider_connection: "stub".to_string(),
                        model: engine_model.to_string(),
                        temperature: None,
                        context_length: engine_context_length,
                        api_base_url: None,
                        api_key: None,
                        reasoning: engine_reasoning.map(str::to_string),
                    },
                )],
            })
        }
    }

    impl Engine for StubEngine {
        fn chat<'a>(
            &'a self,
            _messages: Vec<crate::agent::Message>,
            _tools: Vec<crate::llm::ToolSchema>,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> futures::future::BoxFuture<'a, anyhow::Result<mpsc::Receiver<crate::agent::StreamEvent>>>
        {
            Box::pin(async { anyhow::bail!("the stub engine does not stream") })
        }

        fn config(&self) -> EngineConfig {
            self.config.lock().expect("stub config lock").clone()
        }

        fn list_models(
            &self,
        ) -> futures::future::BoxFuture<'_, anyhow::Result<Vec<crate::llm::ModelInfo>>> {
            let models = self.models.clone();
            Box::pin(async move { Ok(models) })
        }

        fn switch_model(&self, model: &str) -> anyhow::Result<()> {
            self.switched_to
                .lock()
                .expect("stub switch lock")
                .push(model.to_string());
            self.config.lock().expect("stub config lock").model = model.to_string();
            Ok(())
        }

        fn named_engines(&self) -> Vec<(String, EngineConfig)> {
            self.named_engines.clone()
        }
    }

    fn submit(state: &mut AppState, textarea: &mut TextArea<'static>, text: &str) -> Vec<Effect> {
        *textarea = TextArea::from(vec![text.to_string()]);
        configure_textarea(textarea);
        let ctx = textarea_ctx_from(textarea);
        presenter::update(
            state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Enter))),
            &ctx,
        )
    }

    #[test]
    fn clear_and_reset_reach_the_same_handler() {
        let (mut state, mut textarea, _cmd_tx, _cmd_rx) = setup();

        let clear_effects = submit(&mut state, &mut textarea, "/clear");
        let reset_effects = submit(&mut state, &mut textarea, "/reset");

        assert!(clear_effects.contains(&Effect::ClearConversation));
        assert_eq!(clear_effects, reset_effects);
    }

    #[test]
    fn a_command_is_only_recognized_as_the_first_character_of_the_input() {
        let (mut state, mut textarea, _cmd_tx, _cmd_rx) = setup();

        let mid_message = submit(&mut state, &mut textarea, "please run /tools for me");
        assert!(mid_message.contains(&Effect::SubmitPrompt { steer: false }));
        assert!(!mid_message.contains(&Effect::ShowTools));

        let indented = submit(&mut state, &mut textarea, " /tools");
        assert!(!indented.contains(&Effect::ShowTools));
    }

    #[test]
    fn an_unknown_slash_word_is_sent_to_the_model_as_ordinary_input() {
        let (mut state, mut textarea, _cmd_tx, _cmd_rx) = setup();

        let effects = submit(&mut state, &mut textarea, "/foo");

        assert!(effects.contains(&Effect::SubmitPrompt { steer: false }));
        assert!(matches!(
            state.chat_entries.last(),
            Some(crate::tui_reducer_state::ChatEntry::UserMessage { text, .. }) if text == "/foo"
        ));
    }

    #[test]
    fn enter_activates_the_selected_partial_command_instead_of_submitting_it() {
        let (mut state, _textarea, _cmd_tx, _cmd_rx) = setup();
        let mut textarea = TextArea::from(vec!["/model".to_string()]);
        configure_textarea(&mut textarea);
        state.refresh_slash_overlay(&textarea.lines().join("\n"));

        let effects = presenter::update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Enter))),
            &textarea_ctx_from(&textarea),
        );

        assert_eq!(
            effects,
            vec![
                Effect::SetInputText("/models ".to_string()),
                Effect::ListModels { filter: None }
            ],
            "Enter should activate the highlighted /models suggestion"
        );
        assert_eq!(state.slash_overlay, SlashOverlay::Hidden);
        assert!(state.chat_entries.is_empty());
    }

    #[test]
    fn models_command_passes_a_typed_filter_instead_of_submitting_chat() {
        let (mut state, _textarea, _cmd_tx, _cmd_rx) = setup();
        let mut textarea = TextArea::default();

        let effects = submit(&mut state, &mut textarea, "/models deepseek");

        assert_eq!(
            effects,
            vec![
                Effect::SetInputText("/models deepseek".to_string()),
                Effect::ListModels {
                    filter: Some("deepseek".to_string())
                }
            ]
        );
        assert!(
            !effects.contains(&Effect::SubmitPrompt { steer: false }),
            "a model filter is a slash-command argument, not a chat prompt"
        );
    }

    #[tokio::test]
    async fn tab_completes_models_from_the_partial_model_prefix() {
        let (mut state, _textarea, cmd_tx, _cmd_rx) = setup();
        let mut textarea = TextArea::from(vec!["/model".to_string()]);
        configure_textarea(&mut textarea);
        state.refresh_slash_overlay(&textarea.lines().join("\n"));

        let effects = presenter::update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Tab))),
            &textarea_ctx_from(&textarea),
        );
        assert_eq!(effects, vec![Effect::SetInputText("/models".to_string())]);

        execute_effects(
            &mut state,
            &mut textarea,
            &cmd_tx,
            effects,
            &mut MouseDrag::default(),
            &mut NoopClipboard,
            &mut PendingRunIds::default(),
        )
        .await;
        assert_eq!(textarea.lines(), &["/models"]);
    }

    #[test]
    fn the_suggestion_overlay_tracks_the_partially_typed_command() {
        let mut state = AppState::new();

        state.refresh_slash_overlay("/mo");
        let SlashOverlay::Suggestions { choices, selected } = &state.slash_overlay else {
            panic!("expected suggestions, got {:?}", state.slash_overlay);
        };
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0].name, "/models");
        assert_eq!(*selected, 0);

        state.refresh_slash_overlay("hello");
        assert_eq!(state.slash_overlay, SlashOverlay::Hidden);
    }

    #[tokio::test]
    async fn escape_while_command_suggestions_are_open_closes_them_and_clears_the_draft() {
        // Regression: Esc used to only hide the suggestion overlay without
        // touching the draft. Since `refresh_slash_overlay` recomputes the
        // overlay from the draft on every action, the still-matching
        // "/models" text made the very same overlay reopen on that same
        // recompute — Esc looked like a no-op from the user's perspective.
        let (mut state, mut textarea, cmd_tx, _cmd_rx) = setup();
        let mut mouse_drag = MouseDrag::default();
        let mut clipboard = NoopClipboard;
        let mut pending_run_ids = PendingRunIds::default();

        // Type "/models" one character at a time through the real driver
        // path: each key both edits the textarea and re-derives the
        // suggestion overlay from the resulting draft.
        for ch in "/models".chars() {
            let ctx = textarea_ctx_from(&textarea);
            let effects = presenter::update(
                &mut state,
                TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Char(ch)))),
                &ctx,
            );
            execute_effects(
                &mut state,
                &mut textarea,
                &cmd_tx,
                effects,
                &mut mouse_drag,
                &mut clipboard,
                &mut pending_run_ids,
            )
            .await;
            state.refresh_slash_overlay(&textarea.lines().join("\n"));
        }

        assert!(
            matches!(state.slash_overlay, SlashOverlay::Suggestions { .. }),
            "expected the suggestion overlay to be open, got {:?}",
            state.slash_overlay
        );
        assert_eq!(textarea.lines().join("\n"), "/models");

        let ctx = textarea_ctx_from(&textarea);
        let effects = presenter::update(&mut state, TuiEvent::Input(InputEvent::Escape), &ctx);
        execute_effects(
            &mut state,
            &mut textarea,
            &cmd_tx,
            effects,
            &mut mouse_drag,
            &mut clipboard,
            &mut pending_run_ids,
        )
        .await;
        state.refresh_slash_overlay(&textarea.lines().join("\n"));

        assert_eq!(
            state.slash_overlay,
            SlashOverlay::Hidden,
            "Esc must close the suggestion overlay and it must stay closed"
        );
        assert_eq!(
            textarea.lines().join("\n"),
            "",
            "Esc must clear the draft so the overlay cannot immediately reopen"
        );
    }

    #[test]
    fn command_suggestions_navigate_with_up_and_down_and_tab_completes_the_selection() {
        let mut state = AppState::new();
        state.refresh_slash_overlay("/");
        let textarea = TextareaContext {
            is_empty: false,
            text: "/".to_string(),
            cursor_row: 0,
            cursor_row_is_last: true,
        };

        let down = presenter::update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Down))),
            &textarea,
        );
        assert!(down.is_empty());
        assert_eq!(
            state.selected_slash_command().map(|command| command.name),
            Some("/models")
        );

        presenter::update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Down))),
            &textarea,
        );
        assert_eq!(
            state.selected_slash_command().map(|command| command.name),
            Some("/tools")
        );

        presenter::update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Down))),
            &textarea,
        );
        assert_eq!(
            state.selected_slash_command().map(|command| command.name),
            Some("/tools")
        );

        presenter::update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Up))),
            &textarea,
        );
        assert_eq!(
            state.selected_slash_command().map(|command| command.name),
            Some("/models")
        );

        let completion = presenter::update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Tab))),
            &textarea,
        );
        assert_eq!(
            completion,
            vec![Effect::SetInputText("/models".to_string())]
        );
    }

    #[test]
    fn tab_completes_the_only_matching_command_and_is_normal_input_without_suggestions() {
        let mut state = AppState::new();
        state.refresh_slash_overlay("/re");
        let completion = presenter::update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Tab))),
            &TextareaContext {
                is_empty: false,
                text: "/re".to_string(),
                cursor_row: 0,
                cursor_row_is_last: true,
            },
        );
        assert_eq!(completion, vec![Effect::SetInputText("/clear".to_string())]);

        state.slash_overlay = SlashOverlay::Hidden;
        let tab = key_event(KeyCode::Tab);
        let normal_input = presenter::update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(tab)),
            &TextareaContext {
                is_empty: true,
                text: String::new(),
                cursor_row: 0,
                cursor_row_is_last: true,
            },
        );
        assert_eq!(
            normal_input,
            vec![Effect::ExitBrowsing, Effect::TextareaKey(tab)]
        );
    }

    #[tokio::test]
    async fn models_lists_the_candidates_and_preselects_the_active_model() {
        let (mut state, _textarea, cmd_tx, _cmd_rx) = setup();
        let engine: Arc<dyn Engine> = StubEngine::with_models(&["small", "large"], "large");

        execute_slash_effects(
            &mut state,
            vec![Effect::ListModels { filter: None }],
            &engine,
            &cmd_tx,
        )
        .await;

        let SlashOverlay::ModelPicker {
            choices, selected, ..
        } = &state.slash_overlay
        else {
            panic!("expected a model picker, got {:?}", state.slash_overlay);
        };
        assert_eq!(
            choices
                .iter()
                .map(|choice| choice.selection.model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["small", "large"]
        );
        assert_eq!(
            choices
                .iter()
                .map(|choice| choice.label.as_str())
                .collect::<Vec<_>>(),
            vec![
                "stub/small  reasoning: unavailable",
                "stub/large  reasoning: unavailable  — active"
            ]
        );
        assert_eq!(*selected, 1);
    }

    #[tokio::test]
    async fn models_lists_an_engine_shortcut_with_its_catalog_reasoning_efforts() {
        // Reproduces a profile's `[engines.<name>]` table naming a model the
        // catalog *does* advertise reasoning efforts for — the shortcut must
        // show those efforts (and the configured one), not "unavailable".
        let (mut state, _textarea, cmd_tx, _cmd_rx) = setup();
        let engine: Arc<dyn Engine> = StubEngine::with_named_engine(
            &["small", "thinking"],
            "small",
            "openrouter",
            "thinking",
            &["low", "high", "max"],
            Some("max"),
            Some(32_000),
            Some(999_999),
        );

        execute_slash_effects(
            &mut state,
            vec![Effect::ListModels { filter: None }],
            &engine,
            &cmd_tx,
        )
        .await;

        let SlashOverlay::ModelPicker { choices, .. } = &state.slash_overlay else {
            panic!("expected a model picker, got {:?}", state.slash_overlay);
        };
        let shortcut = choices
            .iter()
            .find(|choice| choice.engine_name.is_some())
            .expect("the named engine must appear as a shortcut");
        assert_eq!(
            shortcut.selection.model.reasoning_efforts,
            Some(vec![
                "low".to_string(),
                "high".to_string(),
                "max".to_string()
            ]),
            "the shortcut's model must carry the catalog's advertised reasoning efforts"
        );
        assert_eq!(
            shortcut.selection.model.context_length,
            Some(32_000),
            "the shortcut must show the engine's own configured context_length, \
             not the provider's advertised one (999_999)"
        );
        assert_eq!(
            shortcut.label, "openrouter → stub/thinking  context: 31kb  reasoning: max ⚙",
            "the shortcut must show its configured reasoning effort instead of 'unavailable'"
        );
    }

    #[tokio::test]
    async fn models_lists_an_engine_shortcut_falling_back_to_the_catalogs_context_length() {
        // When the profile's `[engines.<name>]` table doesn't set
        // `context_length`, the shortcut must show the provider's advertised
        // context length for the model instead of silently defaulting.
        let (mut state, _textarea, cmd_tx, _cmd_rx) = setup();
        let engine: Arc<dyn Engine> = StubEngine::with_named_engine(
            &["small", "thinking"],
            "small",
            "openrouter",
            "thinking",
            &["low", "high", "max"],
            Some("max"),
            None,
            Some(999_999),
        );

        execute_slash_effects(
            &mut state,
            vec![Effect::ListModels { filter: None }],
            &engine,
            &cmd_tx,
        )
        .await;

        let SlashOverlay::ModelPicker { choices, .. } = &state.slash_overlay else {
            panic!("expected a model picker, got {:?}", state.slash_overlay);
        };
        let shortcut = choices
            .iter()
            .find(|choice| choice.engine_name.is_some())
            .expect("the named engine must appear as a shortcut");
        assert_eq!(
            shortcut.selection.model.context_length,
            Some(999_999),
            "an unconfigured context_length must fall back to the catalog's advertised value"
        );
        assert_eq!(
            shortcut.label,
            "openrouter → stub/thinking  context: 976kb  reasoning: max ⚙"
        );
    }

    #[tokio::test]
    async fn models_lists_an_engine_shortcut_falling_back_to_the_built_in_default_context_length() {
        // When neither TOML nor the catalog advertise a context length, the
        // shortcut must show the built-in default rather than nothing.
        let (mut state, _textarea, cmd_tx, _cmd_rx) = setup();
        let engine: Arc<dyn Engine> = StubEngine::with_named_engine(
            &["small", "thinking"],
            "small",
            "openrouter",
            "thinking",
            &["low", "high", "max"],
            Some("max"),
            None,
            None,
        );

        execute_slash_effects(
            &mut state,
            vec![Effect::ListModels { filter: None }],
            &engine,
            &cmd_tx,
        )
        .await;

        let SlashOverlay::ModelPicker { choices, .. } = &state.slash_overlay else {
            panic!("expected a model picker, got {:?}", state.slash_overlay);
        };
        let shortcut = choices
            .iter()
            .find(|choice| choice.engine_name.is_some())
            .expect("the named engine must appear as a shortcut");
        assert_eq!(
            shortcut.selection.model.context_length,
            Some(DEFAULT_CONTEXT_LENGTH),
            "an unconfigured, unadvertised context_length must fall back to the built-in default"
        );
    }

    #[tokio::test]
    async fn models_preselects_the_active_model_within_an_initial_filter() {
        let (mut state, _textarea, cmd_tx, _cmd_rx) = setup();
        let engine: Arc<dyn Engine> =
            StubEngine::with_models(&["small", "deepseek-v3", "large"], "large");

        execute_slash_effects(
            &mut state,
            vec![Effect::ListModels {
                filter: Some("DEEPSEEK".to_string()),
            }],
            &engine,
            &cmd_tx,
        )
        .await;

        // The catalog always holds every fetched model — filtering only
        // narrows what is displayed/selectable, so further typing can
        // widen the results again without another provider round-trip.
        let SlashOverlay::ModelPicker {
            choices, filter, ..
        } = &state.slash_overlay
        else {
            panic!(
                "expected a filtered model picker, got {:?}",
                state.slash_overlay
            );
        };
        assert_eq!(
            choices
                .iter()
                .map(|choice| choice.selection.model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["small", "deepseek-v3", "large"]
        );
        assert_eq!(filter, "DEEPSEEK");
        assert_eq!(
            state
                .selected_model()
                .map(|choice| choice.selection.model.id.as_str()),
            Some("deepseek-v3")
        );
    }

    #[tokio::test]
    async fn models_keeps_the_picker_open_when_an_initial_filter_matches_nothing() {
        let (mut state, _textarea, cmd_tx, _cmd_rx) = setup();
        let engine: Arc<dyn Engine> = StubEngine::with_models(&["small", "large"], "large");

        execute_slash_effects(
            &mut state,
            vec![Effect::ListModels {
                filter: Some("missing".to_string()),
            }],
            &engine,
            &cmd_tx,
        )
        .await;

        // No auto-hide/notice on a no-match filter: the picker stays open
        // so the user can keep editing the filter (e.g. backspace) to
        // recover matches, without re-fetching the catalog.
        assert!(matches!(
            state.slash_overlay,
            SlashOverlay::ModelPicker { .. }
        ));
        assert_eq!(state.selected_model(), None);
    }

    #[tokio::test]
    async fn typing_after_models_narrows_the_already_open_picker_without_clearing_the_draft() {
        let (mut state, mut textarea, cmd_tx, _cmd_rx) = setup();
        let engine: Arc<dyn Engine> =
            StubEngine::with_models(&["small", "deepseek-v3", "large"], "large");
        let mut mouse_drag = MouseDrag::default();
        let mut clipboard = NoopClipboard;
        let mut pending_run_ids = PendingRunIds::default();

        // Submit "/models": the picker opens and the draft is normalized to
        // "/models " (not cleared), ready for more typing.
        let effects = submit(&mut state, &mut textarea, "/models");
        let (slash_effects, other_effects) = split_slash_effects(effects);
        execute_effects(
            &mut state,
            &mut textarea,
            &cmd_tx,
            other_effects,
            &mut mouse_drag,
            &mut clipboard,
            &mut pending_run_ids,
        )
        .await;
        execute_slash_effects(&mut state, slash_effects, &engine, &cmd_tx).await;
        state.refresh_slash_overlay(&textarea.lines().join("\n"));

        assert!(matches!(
            state.slash_overlay,
            SlashOverlay::ModelPicker { .. }
        ));
        assert_eq!(textarea.lines().join("\n"), "/models ");

        // Keep typing: each keystroke must land in the still-open picker's
        // filter, not be dropped or start a disconnected fresh draft.
        for ch in "deepseek".chars() {
            let ctx = textarea_ctx_from(&textarea);
            let effects = presenter::update(
                &mut state,
                TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Char(ch)))),
                &ctx,
            );
            execute_effects(
                &mut state,
                &mut textarea,
                &cmd_tx,
                effects,
                &mut mouse_drag,
                &mut clipboard,
                &mut pending_run_ids,
            )
            .await;
            state.refresh_slash_overlay(&textarea.lines().join("\n"));
        }

        assert_eq!(textarea.lines().join("\n"), "/models deepseek");
        let SlashOverlay::ModelPicker {
            choices, filter, ..
        } = &state.slash_overlay
        else {
            panic!("expected the model picker to stay open while typing a live filter");
        };
        assert_eq!(filter, "deepseek");
        // The full catalog is still cached — only the display/selection is
        // narrowed — so no second provider round-trip was needed.
        assert_eq!(choices.len(), 3);
        assert_eq!(
            state
                .selected_model()
                .map(|choice| choice.selection.model.id.as_str()),
            Some("deepseek-v3")
        );

        // Reported regression: Esc while the (still-open, live-filtered)
        // picker is showing must close the popup AND clear the "/models
        // deepseek" residue from the draft — not leave either behind.
        let ctx = textarea_ctx_from(&textarea);
        let effects = presenter::update(&mut state, TuiEvent::Input(InputEvent::Escape), &ctx);
        execute_effects(
            &mut state,
            &mut textarea,
            &cmd_tx,
            effects,
            &mut mouse_drag,
            &mut clipboard,
            &mut pending_run_ids,
        )
        .await;

        assert_eq!(state.slash_overlay, SlashOverlay::Hidden);
        assert_eq!(textarea.lines().join("\n"), "");
    }

    #[test]
    fn model_picker_cycles_reasoning_effort_with_left_and_right() {
        let (mut state, textarea, _cmd_tx, _cmd_rx) = setup();
        let selection = crate::llm::ProviderModelOption {
            provider_connection: "stub".to_string(),
            model: crate::llm::ModelInfo {
                id: "thinking".to_string(),
                context_length: None,
                reasoning_efforts: Some(vec!["low".to_string(), "high".to_string()]),
            },
            reasoning_effort: None,
        };
        state.slash_overlay = SlashOverlay::ModelPicker {
            choices: vec![ModelChoice {
                label: "stub/thinking  reasoning: default".to_string(),
                selection,
                is_active: false,
                engine_name: None,
            }],
            filter: String::new(),
            selected: 0,
        };
        let context = textarea_ctx_from(&textarea);

        let effects = presenter::update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Right))),
            &context,
        );
        assert!(effects.is_empty());
        assert_eq!(
            state
                .selected_model()
                .and_then(|choice| choice.selection.reasoning_effort.as_deref()),
            Some("low")
        );
        assert!(state
            .selected_model()
            .expect("selected model")
            .label
            .contains("reasoning: low"));

        presenter::update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Right))),
            &context,
        );
        assert_eq!(
            state
                .selected_model()
                .and_then(|choice| choice.selection.reasoning_effort.as_deref()),
            Some("high")
        );

        presenter::update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Left))),
            &context,
        );
        assert_eq!(
            state
                .selected_model()
                .and_then(|choice| choice.selection.reasoning_effort.as_deref()),
            Some("low")
        );

        presenter::update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Left))),
            &context,
        );
        assert_eq!(
            state
                .selected_model()
                .expect("selected model")
                .selection
                .reasoning_effort,
            None
        );
    }

    #[tokio::test]
    async fn confirming_a_picked_model_switches_the_engine_and_records_the_change() {
        let (mut state, _textarea, cmd_tx, mut cmd_rx) = setup();
        let stub = StubEngine::with_models(&["small", "large"], "large");
        let engine: Arc<dyn Engine> = stub.clone();

        execute_slash_effects(
            &mut state,
            vec![Effect::ListModels { filter: None }],
            &engine,
            &cmd_tx,
        )
        .await;
        // Move the selection off the active model, then confirm it.
        state.move_slash_selection(-1);
        let picked = state
            .selected_model()
            .expect("a highlighted model")
            .selection
            .clone();
        let ctx = TextareaContext {
            is_empty: true,
            text: String::new(),
            cursor_row: 0,
            cursor_row_is_last: true,
        };
        let effects = presenter::update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Enter))),
            &ctx,
        );
        assert_eq!(
            effects,
            vec![Effect::ClearTextarea, Effect::SwitchModel(picked)]
        );
        assert_eq!(state.slash_overlay, SlashOverlay::Hidden);

        execute_slash_effects(&mut state, effects, &engine, &cmd_tx).await;

        assert_eq!(
            *stub.switched_to.lock().expect("stub switch lock"),
            vec!["small".to_string()]
        );
        assert_eq!(engine.config().model, "small");
        // The confirmation is a TUI-only notice — never injected into the
        // conversation (see `Effect::SwitchModel`'s handler for why a
        // mid-conversation `role:"system"` message is unsafe).
        assert!(matches!(
            state.chat_entries.last(),
            Some(crate::tui_reducer_state::ChatEntry::Notice { text }) if text.contains("small")
        ));
        let command = cmd_rx.try_recv().expect("a model-changed command");
        assert!(matches!(
            command,
            AgentCommand::RecordModelChanged { model, .. } if model == "small"
        ));
        assert!(cmd_rx.try_recv().is_err(), "no further commands are sent");
    }

    #[tokio::test]
    async fn escape_dismisses_the_model_picker_without_switching() {
        let (mut state, textarea, cmd_tx, _cmd_rx) = setup();
        let stub = StubEngine::with_models(&["small", "large"], "large");
        let engine: Arc<dyn Engine> = stub.clone();
        execute_slash_effects(
            &mut state,
            vec![Effect::ListModels { filter: None }],
            &engine,
            &cmd_tx,
        )
        .await;

        let ctx = textarea_ctx_from(&textarea);
        presenter::update(&mut state, TuiEvent::Input(InputEvent::Escape), &ctx);

        assert_eq!(state.slash_overlay, SlashOverlay::Hidden);
        assert!(stub
            .switched_to
            .lock()
            .expect("stub switch lock")
            .is_empty());
    }

    #[tokio::test]
    async fn clear_resets_the_transcript_and_tells_the_agent_to_start_a_new_session() {
        let (mut state, _textarea, cmd_tx, mut cmd_rx) = setup();
        let engine: Arc<dyn Engine> = StubEngine::with_models(&[], "large");
        state.push_user_bubble("earlier question".to_string(), UserMessageState::Sent);

        execute_slash_effects(
            &mut state,
            vec![Effect::ClearConversation],
            &engine,
            &cmd_tx,
        )
        .await;

        assert!(matches!(
            state.chat_entries.as_slice(),
            [crate::tui_reducer_state::ChatEntry::Notice { .. }]
        ));
        // The prompt stays recallable with ↑ even though the transcript is gone.
        assert_eq!(state.input_history, vec!["earlier question".to_string()]);
        assert!(matches!(
            cmd_rx.try_recv().expect("a clear command"),
            AgentCommand::ClearContext
        ));
    }

    #[tokio::test]
    async fn tools_reports_the_registry_names_without_touching_the_agent() {
        let (mut state, _textarea, cmd_tx, mut cmd_rx) = setup();
        let engine: Arc<dyn Engine> = StubEngine::with_models(&[], "large");
        state.available_tool_names = vec!["read_file".to_string(), "run_command".to_string()];

        execute_slash_effects(&mut state, vec![Effect::ShowTools], &engine, &cmd_tx).await;

        let Some(crate::tui_reducer_state::ChatEntry::Notice { text }) = state.chat_entries.last()
        else {
            panic!("expected a notice bubble");
        };
        assert!(text.contains("read_file"));
        assert!(text.contains("run_command"));
        assert!(
            cmd_rx.try_recv().is_err(),
            "/tools must not command the agent"
        );
    }
}
