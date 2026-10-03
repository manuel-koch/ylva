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

use std::io::{self, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen, SetTitle,
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
use crate::session_title::{SessionTitleUpdate, TitleAttempt};
use crate::tui_reducer_state::{
    engine_choice_label, extract_models_filter, matching_model_indices, model_choice_label,
    AppState, ConfirmKey, DomainEvent, Effect, Focus, HistoryDirection, InputEvent, InputSnapshot,
    ModelChoice, SlashOverlay, TuiDisplayMetadata, TuiMode, UserMessageState, CONFIRM_TIMEOUT,
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
/// Returns `None` for tool updates handled directly by the Driver.
fn ui_update_to_domain_event(update: &UiUpdate) -> Option<DomainEvent> {
    match update {
        UiUpdate::StreamStarted => Some(DomainEvent::StreamStarted),
        UiUpdate::AppendText(text) => Some(DomainEvent::StreamChunk(text.clone())),
        UiUpdate::AppendThinking(text) => Some(DomainEvent::StreamThinkingChunk(text.clone())),
        UiUpdate::StreamDone => Some(DomainEvent::StreamDone),
        UiUpdate::StreamCancelled => Some(DomainEvent::StreamCancelled),
        UiUpdate::AgentIdle => Some(DomainEvent::AgentIdle),
        UiUpdate::ContextUsage {
            estimated_tokens,
            context_length,
        } => Some(DomainEvent::ContextUsage {
            estimated_tokens: *estimated_tokens,
            context_length: *context_length,
        }),
        UiUpdate::ChildContextUsage {
            run_id,
            estimated_tokens,
            context_length,
        } => Some(DomainEvent::ChildContextUsage {
            run_id: *run_id,
            estimated_tokens: *estimated_tokens,
            context_length: *context_length,
        }),
        UiUpdate::CompactionApplied {
            entry_index,
            summarized_messages,
        } => Some(DomainEvent::CompactionApplied {
            entry_index: *entry_index,
            summarized_messages: *summarized_messages,
        }),
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
        // ToolError, ToolAutoDenied and Notice are handled directly by the
        // Driver via AppState; no corresponding DomainEvent exists in the
        // Presenter's surface.
        UiUpdate::ToolError { .. } | UiUpdate::ToolAutoDenied { .. } | UiUpdate::Notice { .. } => {
            None
        }
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
            Effect::SaveInputStateAndClear => {
                let lines = textarea.lines().to_vec();
                let input_lines = if lines == vec![String::new()] {
                    None
                } else {
                    Some(lines)
                };
                *textarea = TextArea::default();
                configure_textarea(textarea);
                // Park textarea and overlay as one unit *before* the hide:
                // the rows the overlay draws on are taken over by the
                // approval footer / clarify banner for the pause's duration.
                let slash_overlay = state.slash_overlay.clone();
                state.pending_input_snapshot = Some(InputSnapshot {
                    input_lines,
                    slash_overlay,
                });
                state.slash_overlay = SlashOverlay::Hidden;
            }
            Effect::RestoreInputState => {
                let snapshot = state.pending_input_snapshot.take();
                *textarea = if let Some(InputSnapshot {
                    input_lines: Some(lines),
                    ..
                }) = &snapshot
                {
                    TextArea::from(lines.clone())
                } else {
                    TextArea::default()
                };
                configure_textarea(textarea);
                // Bring the parked overlay back; the driver's following
                // `refresh_slash_overlay` reconciles it with the restored
                // draft (the `/models` picker keeps its fetched catalog).
                if let Some(snapshot) = snapshot {
                    state.slash_overlay = snapshot.slash_overlay;
                }
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
            | Effect::LoadChildTranscript { .. }
            | Effect::ListModels { .. }
            | Effect::SwitchModel(_) => {}
        }
    }
    ActionOutcome::Continue
}

/// Resolve a replay-discovered child transcript before the pure reducer enters
/// the child view. Live children are already populated and pass through
/// unchanged.
fn prepare_child_replay_effects(
    state: &mut AppState,
    effects: Vec<Effect>,
    store: Option<&crate::store::Store>,
) -> Vec<Effect> {
    let mut prepared = Vec::with_capacity(effects.len());
    for effect in effects {
        match effect {
            Effect::LoadChildTranscript { run_id } => {
                if state.child_needs_lazy_load(run_id) {
                    let messages = match store {
                        Some(store) => {
                            let path = crate::store::session_path(
                                store.chats_root(),
                                store.project_key(),
                                run_id,
                            );
                            match crate::store::Store::read_messages(&path) {
                                Ok(messages) => messages,
                                Err(error) => {
                                    tracing::warn!(
                                        %error,
                                        path = %path.display(),
                                        %run_id,
                                        "could not replay child transcript"
                                    );
                                    Vec::new()
                                }
                            }
                        }
                        None => Vec::new(),
                    };
                    state.load_child_messages(run_id, &messages);
                }
                state.switch_to_child_view(run_id);
            }
            other => prepared.push(other),
        }
    }
    prepared
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

/// Resume a `/models` catalog request that a pause (tool approval, clarify)
/// interrupted.
///
/// A result completing while the pause hid the overlay is discarded — the
/// result branch only applies to a visible `SlashOverlay::LoadingModels` —
/// so once `RestoreInputState` puts `LoadingModels` back, the request must
/// be made again for the picker to ever populate. When the original request
/// is still in flight, its pending handle remains and the existing result
/// branch applies it on arrival, so this is a no-op then.
fn resume_interrupted_model_list(
    state: &AppState,
    pending_model_list: &mut Option<PendingModelList>,
    engine: &Arc<dyn Engine>,
    draft: &str,
) {
    if !matches!(state.slash_overlay, SlashOverlay::LoadingModels) {
        return;
    }
    if pending_model_list.is_some() {
        return;
    }
    start_model_list(
        pending_model_list,
        Some(extract_models_filter(draft)),
        engine,
    );
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
                        reasoning_default: catalog_model
                            .and_then(|model| model.reasoning_default.clone()),
                        input_modalities: catalog_model
                            .and_then(|model| model.input_modalities.clone()),
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
    if let UiUpdate::Notice { text, run_id } = update {
        state.push_notice_to(*run_id, text.clone());
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
    let slash_overlay_drag = app.slash_overlay_drag_selection();
    let active_subagent_counts = app.active_subagent_counts();
    let context_usage_text = app.active_context_usage().map(|usage| {
        crate::tui_reducer_state::format_context_usage(usage.estimated_tokens, usage.context_length)
    });
    TuiState {
        display_metadata: app.display_metadata,
        parent_agent_mode: agent_mode,
        agent_mode,
        active_subagent_counts,
        chat_entries: app.chat_entries,
        partial_assistant_text: app.turn_buffer.partial_assistant_text,
        partial_thinking_text: app.turn_buffer.partial_thinking_text,
        thinking_started_at: app.turn_buffer.thinking_started_at,
        partial_thinking_collapsed: app.turn_buffer.partial_thinking_collapsed,
        engine_display: app.engine_display,
        queued_steer_texts: app.queued_steer_texts,
        steer_indicator: app.steer_indicator,
        context_usage_text,
        pending_approval: None,
        pending_approval_is_subagent: false,
        input_history: app.input_history,
        history_index: app.history_index,
        draft: app.draft,
        input_area: input_area_rect,
        toast: app.toast,
        spinner_frame: app.spinner_frame,
        input_scroll_top: app.input_scroll_top,
        slash_overlay: app.slash_overlay,
        slash_overlay_drag,
        slash_overlay_layout: app.layout.slash_overlay,
        help_popup: crate::tui_render_state::HelpPopupState {
            show_help,
            help_scroll_offset: app.help_popup.help_scroll_offset,
            help_scroll_x: app.help_popup.help_scroll_x,
            help_popup_rect: app.layout.help_popup.rect,
            help_content_area: app.layout.help_popup.content_area,
            help_rendered_lines: app.help_popup.help_rendered_lines,
            help_selection: app.help_popup.help_selection,
            help_drag_active: app.help_popup.help_drag_active,
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
            header_rendered_lines: Vec::new(),
            header_rendered_line_prefix_widths: Vec::new(),
            header_selection: None,
            header_drag_active: false,
        },
        conversation: crate::tui_render_state::ConversationListState {
            scroll_offset: app.conversation.scroll_offset,
            scroll_locked: app.conversation.scroll_locked,
            chat_area_height: app.conversation.chat_area_height,
            chat_area: chat_area_rect,
            chat_rendered_lines: app.conversation.chat_rendered_lines,
            chat_rendered_line_prefix_widths: app.conversation.chat_rendered_line_prefix_widths,
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
    agent: &AgentLoop,
    mouse_pos: Option<(u16, u16)>,
) -> anyhow::Result<()>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    state.engine_display = agent.engine_display_info();
    let hint = confirm_hint(state);
    terminal.draw(|frame| {
        render::render(
            frame,
            state,
            textarea,
            hint.as_deref(),
            mouse_pos,
            agent.registry().as_ref(),
        );
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
    action_rx: mpsc::Receiver<AppAction>,
    clipboard: &mut dyn ClipboardSink,
) -> anyhow::Result<TuiState>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let tui_runtime = TuiRuntime::disabled();
    run_tui_with_title(
        terminal,
        agent,
        ui_rx,
        initial_prompt,
        action_rx,
        clipboard,
        tui_runtime,
    )
    .await
}

struct TitleDriver {
    factory: Arc<AgentFactory>,
    profile: ProfileConfig,
    attempt: TitleAttempt,
    title_update_tx: mpsc::UnboundedSender<SessionTitleUpdate>,
}

impl TitleDriver {
    fn trigger(&self, agent: &AgentLoop) {
        if let Some(store) = agent.store() {
            let _ = self.attempt.trigger_with_title_updates(
                store,
                Arc::clone(&self.factory),
                self.profile.clone(),
                self.title_update_tx.clone(),
            );
        }
    }
}

struct TuiRuntime {
    driver: Option<TitleDriver>,
    update_rx: mpsc::UnboundedReceiver<SessionTitleUpdate>,
    display_metadata: TuiDisplayMetadata,
    input_history_limit: usize,
}

impl TuiRuntime {
    fn disabled() -> Self {
        let (_update_tx, update_rx) = mpsc::unbounded_channel();
        Self {
            driver: None,
            update_rx,
            display_metadata: TuiDisplayMetadata::default(),
            input_history_limit: ProfileConfig::default().input_history_limit,
        }
    }
}

fn trigger_title_on_stream_done(
    update: &UiUpdate,
    title_driver: Option<&TitleDriver>,
    agent: &AgentLoop,
    active_title_session_id: &mut Option<uuid::Uuid>,
) {
    if !matches!(update, UiUpdate::StreamDone) {
        return;
    }
    if let Some(title_driver) = title_driver {
        *active_title_session_id = agent.store().map(|store| store.session_id());
        title_driver.trigger(agent);
    }
}

const DEFAULT_TERMINAL_TITLE: &str = "Ylva Agent Harness";

/// Format the terminal title without allowing model output to inject terminal
/// control characters into the title escape sequence.
fn format_terminal_title(session_title: Option<&str>) -> String {
    let sanitized_title = session_title
        .unwrap_or_default()
        .chars()
        .filter(|character| !character.is_control())
        .collect::<String>()
        .trim()
        .to_string();
    if sanitized_title.is_empty() {
        DEFAULT_TERMINAL_TITLE.to_string()
    } else {
        format!("{sanitized_title} - {DEFAULT_TERMINAL_TITLE}")
    }
}

fn set_terminal_title(session_title: Option<&str>) -> io::Result<()> {
    let mut stdout = io::stdout();
    execute!(stdout, SetTitle(format_terminal_title(session_title)))
}

/// Saves and restores the title around the TUI session where the terminal
/// supports the standard xterm title stack controls.
struct TerminalTitleGuard;

impl TerminalTitleGuard {
    fn new() -> io::Result<Self> {
        let mut stdout = io::stdout();
        write!(stdout, "\x1b[22;0t")?;
        stdout.flush()?;
        Ok(Self)
    }
}

impl Drop for TerminalTitleGuard {
    fn drop(&mut self) {
        let result = (|| -> io::Result<()> {
            let mut stdout = io::stdout();
            write!(stdout, "\x1b[23;0t")?;
            stdout.flush()
        })();
        if let Err(error) = result {
            tracing::debug!(%error, "could not restore the terminal title");
        }
    }
}

async fn run_tui_with_title<B>(
    terminal: &mut Terminal<B>,
    agent: AgentLoop,
    ui_rx: mpsc::Receiver<UiUpdate>,
    initial_prompt: Option<String>,
    mut action_rx: mpsc::Receiver<AppAction>,
    clipboard: &mut dyn ClipboardSink,
    tui_runtime: TuiRuntime,
) -> anyhow::Result<TuiState>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let mut agent = agent;
    let mut ui_rx = ui_rx;
    let TuiRuntime {
        driver: title_driver,
        update_rx: mut title_update_rx,
        display_metadata,
        input_history_limit,
    } = tui_runtime;
    let mut state = AppState::new();
    state.display_metadata = display_metadata;
    let mut textarea = TextArea::default();
    configure_textarea(&mut textarea);
    let replay_store = agent.store();
    if let Some(store) = replay_store.as_ref() {
        // One read rebuilds both the transcript and the compaction marker's
        // position; a snapshot written before the transcript-relative
        // boundary existed simply replays without a marker.
        let messages = match crate::store::Store::read_session_state(store.path()) {
            Ok(session_state) => {
                state.replay_compaction_boundary = session_state.compaction_transcript_boundary;
                session_state.messages
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    path = %store.path().display(),
                    "could not replay TUI session messages"
                );
                Vec::new()
            }
        };
        let child_runs = match crate::store::Store::read_child_runs(store.path()) {
            Ok(child_runs) => child_runs,
            Err(error) => {
                tracing::warn!(
                    %error,
                    path = %store.path().display(),
                    "could not replay child-run lifecycle records"
                );
                Vec::new()
            }
        };
        state.replay_messages(&messages, &child_runs, state.replay_compaction_boundary);
        match store.read_past_explicit_user_messages(input_history_limit) {
            Ok(prior_messages) => state.prepend_input_history(prior_messages),
            Err(error) => {
                tracing::warn!(
                    %error,
                    path = %store.path().display(),
                    "could not load prior session input history"
                );
            }
        }
    }
    let cmd_tx = agent.command_sender();
    let engine = agent.engine();
    state.engine_display = agent.engine_display_info();
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
    let mut title_updates_open = true;
    let mut active_title_session_id = replay_store.as_ref().map(|store| store.session_id());

    let handle = agent.run();

    if let Some(title_driver) = &title_driver {
        title_driver.trigger(&agent);
    }

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
        draw_frame(terminal, &mut state, &mut textarea, &agent, mouse_pos)?;

        let textarea_ctx = TextareaContext {
            is_empty: textarea.lines() == [""],
            text: textarea.lines().join("\n"),
            cursor_row: textarea.cursor().0,
            cursor_row_is_last: textarea.cursor().0 + 1 >= textarea.lines().len().max(1),
        };

        select! {
            title_update = title_update_rx.recv(), if title_updates_open => {
                match title_update {
                    Some(update) if active_title_session_id == Some(update.session_id) => {
                        if let Err(error) = set_terminal_title(Some(&update.title)) {
                            tracing::warn!(
                                %error,
                                "could not update the terminal title with the session title"
                            );
                        }
                    }
                    Some(update) => {
                        tracing::debug!(
                            session_id = %update.session_id,
                            stage = "stale_terminal_update",
                            "ignored a session-title update for an inactive session"
                        );
                    }
                    None => {
                        title_updates_open = false;
                    }
                }
            }

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
                    let effects =
                        prepare_child_replay_effects(&mut state, effects, replay_store.as_ref());
                    if execute_effects(
                        &mut state, &mut textarea, &cmd_tx, effects, &mut mouse_drag, clipboard,
                        &mut pending_run_ids,
                    )
                    .await
                        == ActionOutcome::Quit
                    {
                        break;
                    }
                    // Mouse actions can change the draft too — the approval
                    // approve/deny buttons restore the snapshot saved when the
                    // pause started — so mirror the key-action path below:
                    // resume a discarded catalog request, then re-derive the
                    // suggestion overlay from the restored draft.
                    resume_interrupted_model_list(
                        &state,
                        &mut pending_model_list,
                        &engine,
                        &textarea.lines().join("\n"),
                    );
                    state.refresh_slash_overlay(&textarea.lines().join("\n"));
                    continue;
                }

                let Some(event) = app_action_to_tui_event(action) else { continue };
                let was_loading_models = matches!(state.slash_overlay, SlashOverlay::LoadingModels);
                let effects = presenter::update(&mut state, event, &textarea_ctx);
                // Slash-command effects are the only ones needing the engine,
                // and only key input can produce them.
                let (slash_effects, effects) = split_slash_effects(effects);
                let effects =
                    prepare_child_replay_effects(&mut state, effects, replay_store.as_ref());
                if execute_effects(
                    &mut state, &mut textarea, &cmd_tx, effects, &mut mouse_drag, clipboard,
                    &mut pending_run_ids,
                )
                .await
                    == ActionOutcome::Quit
                {
                    break;
                }
                // A pause that just restored `SlashOverlay::LoadingModels`
                // needs its discarded catalog request made again.
                resume_interrupted_model_list(
                    &state,
                    &mut pending_model_list,
                    &engine,
                    &textarea.lines().join("\n"),
                );
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
                let clear_conversation_requested = immediate_slash_effects
                    .iter()
                    .any(|effect| matches!(effect, Effect::ClearConversation));
                execute_slash_effects(
                    &mut state,
                    immediate_slash_effects,
                    &engine,
                    &cmd_tx,
                )
                .await;
                if clear_conversation_requested && title_driver.is_some() {
                    active_title_session_id = None;
                    if let Err(error) = set_terminal_title(None) {
                        tracing::warn!(%error, "could not reset the terminal title");
                    }
                }
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
                trigger_title_on_stream_done(
                    &update,
                    title_driver.as_ref(),
                    &agent,
                    &mut active_title_session_id,
                );

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
                            trigger_title_on_stream_done(
                                &next,
                                title_driver.as_ref(),
                                &agent,
                                &mut active_title_session_id,
                            );
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
                let effects =
                    prepare_child_replay_effects(&mut state, effects, replay_store.as_ref());
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
                let effects =
                    prepare_child_replay_effects(&mut state, effects, replay_store.as_ref());
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
    display_metadata: TuiDisplayMetadata,
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
            display_metadata: TuiDisplayMetadata::default(),
        }
    }

    /// Set the runtime identity and paths shown by the TUI.
    pub fn with_display_metadata(mut self, display_metadata: TuiDisplayMetadata) -> Self {
        self.display_metadata = display_metadata;
        self
    }
}

impl App for TuiApp {
    async fn run(self) -> anyhow::Result<u8> {
        let _terminal_title_guard = TerminalTitleGuard::new()?;
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(
            stdout,
            EnterAlternateScreen,
            Hide,
            EnableMouseCapture,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
            EnableBracketedPaste,
            SetTitle(format_terminal_title(None)),
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
        let initial_session_title = agent.store().map(|store| store.title());
        set_terminal_title(initial_session_title.as_deref())?;
        let (title_update_tx, title_update_rx) = mpsc::unbounded_channel();
        let title_driver = Some(TitleDriver {
            factory: Arc::clone(&self.factory),
            profile: self.profile.clone(),
            attempt: TitleAttempt::default(),
            title_update_tx,
        });

        run_tui_with_title(
            &mut terminal,
            agent,
            ui_rx,
            self.explicit_prompt,
            action_rx,
            &mut ArboardClipboard,
            TuiRuntime {
                driver: title_driver,
                update_rx: title_update_rx,
                display_metadata: self.display_metadata,
                input_history_limit: self.profile.input_history_limit,
            },
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

        compact_target_ratio: 0.5,
        soft_compaction_threshold: 0.7,
        hard_compaction_threshold: 0.9,
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
    use std::collections::HashMap;

    use crate::llm::engine::DEFAULT_CONTEXT_LENGTH;
    use crate::tui_reducer_state::{ChatEntry, ToolCallState};
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

    #[test]
    fn terminal_title_uses_the_session_title_and_product_suffix() {
        assert_eq!(
            format_terminal_title(Some("Investigate persistence")),
            "Investigate persistence - Ylva Agent Harness"
        );
        assert_eq!(format_terminal_title(None), "Ylva Agent Harness");
    }

    #[test]
    fn terminal_title_removes_control_characters_from_model_output() {
        assert_eq!(
            format_terminal_title(Some("Title\u{001b} \nwith controls")),
            "Title with controls - Ylva Agent Harness"
        );
    }

    #[tokio::test]
    async fn tui_triggers_title_generation_when_stream_done_is_drained() {
        use ratatui::backend::TestBackend;

        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let store = crate::store::Store::create(
            &chats_root,
            temp.path(),
            Uuid::new_v4(),
            "mock",
            "conversation",
            None,
        )
        .expect("store created");
        let provider = Arc::new(crate::llm::MockProvider::new(
            (0..8).map(|_| "answer ".to_string()).collect(),
            Duration::ZERO,
        ));
        let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
            provider,
            EngineConfig {
                provider_connection: "mock".to_string(),
                model: "conversation".to_string(),
                temperature: None,
                context_length: None,
                api_base_url: None,
                api_key: None,
                reasoning: None,

                compact_target_ratio: 0.5,
                soft_compaction_threshold: 0.7,
                hard_compaction_threshold: 0.9,
            },
        ));
        let factory = Arc::new(
            AgentFactory::new(HashMap::from([("default".to_string(), engine)]))
                .with_allowed_tools(&[])
                .with_store(store.clone()),
        );
        let profile = ProfileConfig::default();
        let (agent, ui_rx) = factory
            .create(&profile, "default", Role::Root)
            .expect("root agent created");
        let (action_tx, action_rx) = mpsc::channel(16);
        let (title_update_tx, title_update_rx) = mpsc::unbounded_channel();
        let title_driver = TitleDriver {
            factory: Arc::clone(&factory),
            profile,
            attempt: TitleAttempt::default(),
            title_update_tx,
        };
        let expected_title = "answer ".repeat(8).trim().to_string();
        let stopper_expected_title = expected_title.clone();
        let title_store = store.clone();
        let stopper = tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
            loop {
                if title_store.title() == stopper_expected_title {
                    action_tx
                        .send(AppAction::CtrlC)
                        .await
                        .expect("first quit confirmation is delivered");
                    action_tx
                        .send(AppAction::CtrlC)
                        .await
                        .expect("second quit confirmation is delivered");
                    return true;
                }
                if tokio::time::Instant::now() >= deadline {
                    action_tx
                        .send(AppAction::CtrlC)
                        .await
                        .expect("first timeout quit confirmation is delivered");
                    action_tx
                        .send(AppAction::CtrlC)
                        .await
                        .expect("second timeout quit confirmation is delivered");
                    return false;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        });
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal created");

        let result = run_tui_with_title(
            &mut terminal,
            agent,
            ui_rx,
            Some("question".to_string()),
            action_rx,
            &mut NoopClipboard,
            TuiRuntime {
                driver: Some(title_driver),
                update_rx: title_update_rx,
                display_metadata: TuiDisplayMetadata::default(),
                input_history_limit: ProfileConfig::default().input_history_limit,
            },
        )
        .await;

        assert!(result.is_ok(), "TUI should exit cleanly");
        assert!(
            stopper.await.expect("stopper task completed"),
            "title generation should complete before the TUI is stopped"
        );
        assert_eq!(store.title(), expected_title);
    }

    #[tokio::test]
    async fn tui_loads_prior_session_user_messages_into_input_history() {
        use ratatui::backend::TestBackend;

        let temp = tempfile::tempdir().expect("tempdir");
        let chats_root = temp.path().join("chats");
        let previous_store = crate::store::Store::create(
            &chats_root,
            temp.path(),
            Uuid::new_v4(),
            "mock",
            "conversation",
            None,
        )
        .expect("previous store created");
        previous_store.record_message(&crate::agent::Message::User {
            content: "previous prompt".to_string(),
            kind: crate::agent::UserMessageKind::Explicit,
        });
        previous_store.record_message(&crate::agent::Message::User {
            content: "latest previous prompt".to_string(),
            kind: crate::agent::UserMessageKind::Explicit,
        });
        previous_store.flush().await;

        let current_store = crate::store::Store::create(
            &chats_root,
            temp.path(),
            Uuid::new_v4(),
            "mock",
            "conversation",
            None,
        )
        .expect("current store created");
        current_store.record_message(&crate::agent::Message::User {
            content: "current prompt".to_string(),
            kind: crate::agent::UserMessageKind::Explicit,
        });
        current_store.flush().await;

        let provider = Arc::new(crate::llm::MockProvider::new(Vec::new(), Duration::ZERO));
        let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
            provider,
            EngineConfig {
                provider_connection: "mock".to_string(),
                model: "conversation".to_string(),
                temperature: None,
                context_length: None,
                api_base_url: None,
                api_key: None,
                reasoning: None,

                compact_target_ratio: 0.5,
                soft_compaction_threshold: 0.7,
                hard_compaction_threshold: 0.9,
            },
        ));
        let factory = AgentFactory::new(HashMap::from([("default".to_string(), engine)]))
            .with_allowed_tools(&[])
            .with_store(current_store);
        let (agent, ui_rx) = factory
            .create(&ProfileConfig::default(), "default", Role::Root)
            .expect("root agent created");
        let (action_tx, action_rx) = mpsc::channel(1);
        drop(action_tx);
        let (_title_update_tx, title_update_rx) = mpsc::unbounded_channel();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal created");

        let tui_state = run_tui_with_title(
            &mut terminal,
            agent,
            ui_rx,
            None,
            action_rx,
            &mut NoopClipboard,
            TuiRuntime {
                driver: None,
                update_rx: title_update_rx,
                display_metadata: TuiDisplayMetadata::default(),
                input_history_limit: 1,
            },
        )
        .await
        .expect("TUI exits cleanly");

        assert_eq!(
            tui_state.input_history,
            vec![
                "latest previous prompt".to_string(),
                "current prompt".to_string()
            ]
        );
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

    #[tokio::test]
    async fn compaction_applied_update_inserts_the_transcript_marker() {
        let (mut state, mut textarea, cmd_tx, _cmd_rx) = setup();
        state.chat_entries.push(ChatEntry::UserMessage {
            text: "old turn".to_string(),
            state: UserMessageState::Sent,
        });
        state.chat_entries.push(ChatEntry::AssistantMessage {
            text: "old reply".to_string(),
            interrupted: false,
        });
        state.chat_entries.push(ChatEntry::UserMessage {
            text: "new turn".to_string(),
            state: UserMessageState::Sent,
        });

        let textarea_context = textarea_ctx_from(&textarea);
        let outcome = process_one_update(
            &mut state,
            &mut textarea,
            &cmd_tx,
            &textarea_context,
            &UiUpdate::CompactionApplied {
                entry_index: 2,
                summarized_messages: 2,
            },
            &mut MouseDrag::default(),
            &mut NoopClipboard,
            &mut PendingRunIds::default(),
        )
        .await;

        assert_eq!(outcome, ActionOutcome::Continue);
        assert_eq!(
            state.chat_entries[2],
            crate::tui_reducer_state::ChatEntry::CompactionMarker {
                summarized_messages: 2
            },
            "the marker lands where the tail begins: {:?}",
            state.chat_entries
        );
        assert!(matches!(
            &state.chat_entries[3],
            ChatEntry::UserMessage { text, .. } if text == "new turn"
        ));

        // A repeated emission of the same boundary must not stack markers.
        let repeat_context = textarea_ctx_from(&textarea);
        process_one_update(
            &mut state,
            &mut textarea,
            &cmd_tx,
            &repeat_context,
            &UiUpdate::CompactionApplied {
                entry_index: 2,
                summarized_messages: 2,
            },
            &mut MouseDrag::default(),
            &mut NoopClipboard,
            &mut PendingRunIds::default(),
        )
        .await;
        assert_eq!(
            state
                .chat_entries
                .iter()
                .filter(|entry| matches!(entry, ChatEntry::CompactionMarker { .. }))
                .count(),
            1,
            "the same boundary is marked once"
        );
    }

    #[tokio::test]
    async fn context_usage_update_stores_the_footer_readout() {
        let (mut state, mut textarea, cmd_tx, _cmd_rx) = setup();
        assert_eq!(state.context_usage, None, "no report before the first turn");

        let textarea_context = textarea_ctx_from(&textarea);
        let outcome = process_one_update(
            &mut state,
            &mut textarea,
            &cmd_tx,
            &textarea_context,
            &UiUpdate::ContextUsage {
                estimated_tokens: 64_200,
                context_length: 128_000,
            },
            &mut MouseDrag::default(),
            &mut NoopClipboard,
            &mut PendingRunIds::default(),
        )
        .await;

        assert_eq!(outcome, ActionOutcome::Continue);
        assert_eq!(
            state.context_usage,
            Some(crate::tui_reducer_state::ContextUsage {
                estimated_tokens: 64_200,
                context_length: 128_000,
            }),
            "the readout is stored as reported, not pre-formatted"
        );
        assert_eq!(
            crate::tui_reducer_state::format_context_usage(64_200, 128_000),
            "64.2K (50%)"
        );

        // A later report replaces the earlier one, in any mode.
        state.mode = TuiMode::Streaming;
        let streaming_context = textarea_ctx_from(&textarea);
        process_one_update(
            &mut state,
            &mut textarea,
            &cmd_tx,
            &streaming_context,
            &UiUpdate::ContextUsage {
                estimated_tokens: 1_000,
                context_length: 128_000,
            },
            &mut MouseDrag::default(),
            &mut NoopClipboard,
            &mut PendingRunIds::default(),
        )
        .await;
        assert_eq!(
            state.context_usage,
            Some(crate::tui_reducer_state::ContextUsage {
                estimated_tokens: 1_000,
                context_length: 128_000,
            })
        );
        assert_eq!(state.mode, TuiMode::Streaming, "the mode is untouched");
    }

    #[test]
    fn child_context_usage_update_projects_for_active_child_without_replacing_root() {
        let (mut state, _textarea, _cmd_tx, _cmd_rx) = setup();
        let run_id = uuid::Uuid::new_v4();
        state.start_child_run(crate::tui_reducer_state::ChildRunInfo {
            run_id,
            bubble_id: uuid::Uuid::new_v4(),
            agent_type: "reviewer".to_string(),
            task: "Review the change".to_string(),
            label: "Review change".to_string(),
        });
        state.context_usage = Some(crate::tui_reducer_state::ContextUsage {
            estimated_tokens: 12_000,
            context_length: 128_000,
        });
        state.switch_to_child_view(run_id);

        let event = ui_update_to_domain_event(&UiUpdate::ChildContextUsage {
            run_id,
            estimated_tokens: 4_000,
            context_length: 32_000,
        })
        .expect("child usage update maps to a domain event");
        presenter::update(
            &mut state,
            TuiEvent::Domain(event),
            &textarea_ctx_from(&TextArea::default()),
        );

        assert_eq!(
            state.context_usage,
            Some(crate::tui_reducer_state::ContextUsage {
                estimated_tokens: 12_000,
                context_length: 128_000,
            }),
            "child usage must not replace the root readout"
        );
        assert_eq!(
            state.active_context_usage(),
            Some(crate::tui_reducer_state::ContextUsage {
                estimated_tokens: 4_000,
                context_length: 32_000,
            }),
            "the active child view selects its own readout"
        );
        assert_eq!(
            crate::tui_reducer_state::format_context_usage(4_000, 32_000),
            "4.0K (13%)"
        );
    }

    #[tokio::test]
    async fn modified_enter_inserts_a_newline_into_the_textarea() {
        for modifier in [KeyModifiers::SHIFT, KeyModifiers::ALT] {
            let (mut state, _initial_textarea, cmd_tx, _cmd_rx) = setup();
            let mut textarea = TextArea::from(vec!["first line".to_string()]);
            configure_textarea(&mut textarea);
            textarea.move_cursor(ratatui_textarea::CursorMove::End);
            let key_event = KeyEvent::new(KeyCode::Enter, modifier);
            let effects = presenter::update(
                &mut state,
                TuiEvent::Input(InputEvent::KeyInput(key_event)),
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

            assert_eq!(
                textarea.lines(),
                &["first line", ""],
                "modifier {modifier:?} should insert a newline"
            );
        }
    }

    #[tokio::test]
    async fn legacy_shift_enter_encoding_inserts_a_newline_into_the_textarea() {
        let (mut state, _initial_textarea, cmd_tx, _cmd_rx) = setup();
        let mut textarea = TextArea::from(vec!["first line".to_string()]);
        configure_textarea(&mut textarea);
        textarea.move_cursor(ratatui_textarea::CursorMove::End);

        let action = crate::events::map_event(crossterm::event::Event::Key(KeyEvent::new(
            KeyCode::Char('j'),
            KeyModifiers::CONTROL,
        )))
        .expect("line-feed key event should be mapped");
        let event = app_action_to_tui_event(action).expect("key action should become a TUI event");
        let effects = presenter::update(&mut state, event, &textarea_ctx_from(&textarea));

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

        assert_eq!(textarea.lines(), &["first line", ""]);
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
    async fn aborting_tool_error_waits_for_idle_before_quitting() {
        let (mut state, mut textarea, cmd_tx, _cmd_rx) = setup();
        let tool_id = Uuid::new_v4();
        state.mode = TuiMode::Aborting;
        state.aborting_exit = true;
        state.focus = Focus::ToolApproval {
            tool_id,
            tool_name: "bash".to_string(),
        };
        state.chat_entries.push(ChatEntry::ToolBubble {
            id: tool_id,
            name: "bash".to_string(),
            state: Box::new(ToolCallState::Pending {
                arguments: serde_json::json!({"command": "echo test"}),
                content_sections: vec![],
                title_arguments: vec![],
            }),
            collapsed: false,
        });

        let textarea_context = textarea_ctx_from(&textarea);
        let outcome = process_one_update(
            &mut state,
            &mut textarea,
            &cmd_tx,
            &textarea_context,
            &UiUpdate::ToolError {
                id: tool_id,
                error: "aborted by user".to_string(),
                duration: Duration::ZERO,
                content_sections: vec![],
                run_id: None,
            },
            &mut MouseDrag::default(),
            &mut NoopClipboard,
            &mut PendingRunIds::default(),
        )
        .await;

        assert_eq!(outcome, ActionOutcome::Continue);
        assert_eq!(
            state.mode,
            TuiMode::Aborting,
            "the terminal tool error must not bypass the aborting state"
        );

        let idle_outcome = process_one_update(
            &mut state,
            &mut textarea,
            &cmd_tx,
            &textarea_context,
            &UiUpdate::AgentIdle,
            &mut MouseDrag::default(),
            &mut NoopClipboard,
            &mut PendingRunIds::default(),
        )
        .await;
        assert_eq!(
            idle_outcome,
            ActionOutcome::Quit,
            "the following idle event should complete a Ctrl-C abort"
        );
    }

    #[tokio::test]
    async fn aborting_tool_error_then_idle_keeps_tui_open_for_escape() {
        let (mut state, mut textarea, cmd_tx, _cmd_rx) = setup();
        let tool_id = Uuid::new_v4();
        state.mode = TuiMode::Aborting;
        state.focus = Focus::ToolApproval {
            tool_id,
            tool_name: "bash".to_string(),
        };
        state.chat_entries.push(ChatEntry::ToolBubble {
            id: tool_id,
            name: "bash".to_string(),
            state: Box::new(ToolCallState::Pending {
                arguments: serde_json::json!({"command": "echo test"}),
                content_sections: vec![],
                title_arguments: vec![],
            }),
            collapsed: false,
        });

        let textarea_context = textarea_ctx_from(&textarea);
        let tool_error_outcome = process_one_update(
            &mut state,
            &mut textarea,
            &cmd_tx,
            &textarea_context,
            &UiUpdate::ToolError {
                id: tool_id,
                error: "aborted by user".to_string(),
                duration: Duration::ZERO,
                content_sections: vec![],
                run_id: None,
            },
            &mut MouseDrag::default(),
            &mut NoopClipboard,
            &mut PendingRunIds::default(),
        )
        .await;
        assert_eq!(tool_error_outcome, ActionOutcome::Continue);

        let idle_outcome = process_one_update(
            &mut state,
            &mut textarea,
            &cmd_tx,
            &textarea_context,
            &UiUpdate::AgentIdle,
            &mut MouseDrag::default(),
            &mut NoopClipboard,
            &mut PendingRunIds::default(),
        )
        .await;
        assert_eq!(idle_outcome, ActionOutcome::Continue);
        assert_eq!(state.mode, TuiMode::Idle);
        assert_eq!(state.focus, Focus::MainInput);
        assert!(!state.aborting_exit);
    }

    #[tokio::test]
    async fn engine_notices_render_as_transcript_notices() {
        let (mut state, mut textarea, cmd_tx, _cmd_rx) = setup();
        let textarea_context = textarea_ctx_from(&textarea);

        let outcome = process_one_update(
            &mut state,
            &mut textarea,
            &cmd_tx,
            &textarea_context,
            &UiUpdate::Notice {
                text: "Model mock/test-model does not advertise reasoning effort \"max\"; using the provider default.".to_string(),
                run_id: None,
            },
            &mut MouseDrag::default(),
            &mut NoopClipboard,
            &mut PendingRunIds::default(),
        )
        .await;

        assert_eq!(outcome, ActionOutcome::Continue);
        let Some(ChatEntry::Notice { text }) = state.chat_entries.last() else {
            panic!("expected a notice bubble, got {:?}", state.chat_entries);
        };
        assert!(
            text.contains("does not advertise reasoning effort"),
            "the notice must carry the engine's wording: {text}"
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
            state.pending_input_snapshot,
            Some(InputSnapshot {
                input_lines: Some(vec!["hello world".to_string()]),
                slash_overlay: SlashOverlay::Hidden,
            })
        );
        assert_eq!(textarea.lines(), &[""]);

        // Restore
        execute_effects(
            &mut state,
            &mut textarea,
            &cmd_tx,
            vec![Effect::RestoreInputState],
            &mut MouseDrag::default(),
            &mut NoopClipboard,
            &mut PendingRunIds::default(),
        )
        .await;
        assert_eq!(textarea.lines(), &["hello world"]);
        assert_eq!(state.pending_input_snapshot, None);
    }

    #[tokio::test]
    async fn approval_hides_the_suggestion_overlay_and_reopens_it_when_resolved() {
        // Regression (backlog): typing a slash command and then receiving a
        // tool approval. Arming the approval must hide the overlay,
        // the per-action recompute must keep it hidden while the approval is
        // pending, and resolving the approval must bring it back together
        // with the restored draft.
        let (mut state, _, cmd_tx, mut cmd_rx) = setup();
        let mut mouse_drag = MouseDrag::default();
        let mut clipboard = NoopClipboard;
        let mut pending_run_ids = PendingRunIds::default();

        let mut textarea = TextArea::from(vec!["/mo".to_string()]);
        configure_textarea(&mut textarea);
        state.refresh_slash_overlay(&textarea.lines().join("\n"));
        assert!(matches!(
            state.slash_overlay,
            SlashOverlay::Suggestions { .. }
        ));

        let tool_id = Uuid::new_v4();
        let effects = presenter::update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolPending {
                id: tool_id,
                name: "write".to_string(),
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
            &mut mouse_drag,
            &mut clipboard,
            &mut pending_run_ids,
        )
        .await;
        assert_eq!(state.slash_overlay, SlashOverlay::Hidden);
        assert_eq!(textarea.lines(), &[""]);

        // Typing a denial reason containing `/` must not reopen the overlay
        // while the approval is pending: the Driver recomputes it after every
        // key action.
        let effects = presenter::update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Char('/')))),
            &textarea_ctx_from(&textarea),
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
        assert_eq!(
            state.slash_overlay,
            SlashOverlay::Hidden,
            "the recompute must not reopen the overlay during the approval"
        );

        let effects = presenter::update(
            &mut state,
            TuiEvent::Input(InputEvent::Escape),
            &textarea_ctx_from(&textarea),
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
        assert_eq!(
            cmd_rx.try_recv(),
            Ok(AgentCommand::DenyTool {
                id: tool_id,
                reason: String::new(),
            })
        );
        assert_eq!(
            textarea.lines(),
            &["/mo"],
            "the snapshotted draft must be restored after the approval"
        );
        state.refresh_slash_overlay(&textarea.lines().join("\n"));
        assert!(
            matches!(state.slash_overlay, SlashOverlay::Suggestions { .. }),
            "the overlay must reopen from the restored draft, got {:?}",
            state.slash_overlay
        );
    }

    #[tokio::test]
    async fn mouse_approve_restores_the_draft_and_reopens_the_suggestion_overlay() {
        // The key-action path recomputes the overlay after its effects; the
        // mouse path must do the same so approving with the button restores
        // the slash-command draft *and* its suggestion overlay.
        let (mut state, _, cmd_tx, mut cmd_rx) = setup();
        let mut mouse_drag = MouseDrag::default();
        let mut clipboard = NoopClipboard;
        let mut pending_run_ids = PendingRunIds::default();

        let mut textarea = TextArea::from(vec!["/mo".to_string()]);
        configure_textarea(&mut textarea);
        state.refresh_slash_overlay(&textarea.lines().join("\n"));
        assert!(matches!(
            state.slash_overlay,
            SlashOverlay::Suggestions { .. }
        ));

        let tool_id = Uuid::new_v4();
        let effects = presenter::update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolPending {
                id: tool_id,
                name: "write".to_string(),
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
            &mut mouse_drag,
            &mut clipboard,
            &mut pending_run_ids,
        )
        .await;
        assert_eq!(state.slash_overlay, SlashOverlay::Hidden);

        state.layout.approve_button = ratatui::layout::Rect::new(1, 1, 12, 1);
        let effects = presenter::update(
            &mut state,
            TuiEvent::Mouse(crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: 2,
                row: 1,
                modifiers: KeyModifiers::NONE,
            }),
            &textarea_ctx_from(&textarea),
        );
        assert_eq!(
            effects,
            vec![
                Effect::ApproveTool { id: tool_id },
                Effect::RestoreInputState
            ]
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
        assert_eq!(
            cmd_rx.try_recv(),
            Ok(AgentCommand::ApproveTool { id: tool_id })
        );
        // Mirrors the Driver's mouse branch.
        state.refresh_slash_overlay(&textarea.lines().join("\n"));
        assert_eq!(textarea.lines(), &["/mo"]);
        assert!(
            matches!(state.slash_overlay, SlashOverlay::Suggestions { .. }),
            "the overlay must reopen from the restored draft, got {:?}",
            state.slash_overlay
        );
    }

    #[tokio::test]
    async fn approval_resolution_restores_the_models_picker_with_its_catalog() {
        // Regression: accepting `/models` opened the picker, then a tool
        // approval arrived. Hiding the overlay for the approval used to
        // destroy the fetched catalog — the draft text alone cannot
        // reproduce it — so resolving the approval fell back to a
        // suggestion list instead of the picker.
        let (mut state, _, cmd_tx, mut cmd_rx) = setup();
        let mut mouse_drag = MouseDrag::default();
        let mut clipboard = NoopClipboard;
        let mut pending_run_ids = PendingRunIds::default();

        let mut textarea = TextArea::from(vec!["/models deep".to_string()]);
        configure_textarea(&mut textarea);
        state.slash_overlay = SlashOverlay::ModelPicker {
            choices: vec![ModelChoice {
                label: "stub/deepseek-v3".to_string(),
                selection: crate::llm::ProviderModelOption {
                    provider_connection: "stub".to_string(),
                    model: crate::llm::ModelInfo {
                        id: "deepseek-v3".to_string(),
                        context_length: None,
                        reasoning_efforts: None,
                        reasoning_default: None,
                        input_modalities: None,
                    },
                    reasoning_effort: None,
                },
                is_active: true,
                engine_name: None,
            }],
            filter: "deep".to_string(),
            selected: 0,
        };

        let tool_id = Uuid::new_v4();
        let effects = presenter::update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolPending {
                id: tool_id,
                name: "write".to_string(),
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
            &mut mouse_drag,
            &mut clipboard,
            &mut pending_run_ids,
        )
        .await;
        assert_eq!(state.slash_overlay, SlashOverlay::Hidden);
        let parked = state
            .pending_input_snapshot
            .clone()
            .expect("input state must be parked while the approval is pending");
        assert!(
            matches!(parked.slash_overlay, SlashOverlay::ModelPicker { .. }),
            "the picker must be parked, got {:?}",
            parked.slash_overlay
        );
        assert_eq!(parked.input_lines, Some(vec!["/models deep".to_string()]));

        let effects = presenter::update(
            &mut state,
            TuiEvent::Input(InputEvent::Escape),
            &textarea_ctx_from(&textarea),
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
        assert_eq!(
            cmd_rx.try_recv(),
            Ok(AgentCommand::DenyTool {
                id: tool_id,
                reason: String::new(),
            })
        );
        assert_eq!(state.pending_input_snapshot, None);
        assert_eq!(textarea.lines(), &["/models deep"]);
        // The Driver's post-effect recompute reconciles the restored picker
        // with the restored draft (same filter, kept catalog).
        state.refresh_slash_overlay(&textarea.lines().join("\n"));
        let SlashOverlay::ModelPicker {
            choices, filter, ..
        } = &state.slash_overlay
        else {
            panic!(
                "the models picker must come back with the restored draft, got {:?}",
                state.slash_overlay
            );
        };
        assert_eq!(filter, "deep");
        assert_eq!(choices.len(), 1);
    }

    #[tokio::test]
    async fn approval_input_never_interprets_slash_commands() {
        // The approval popup's input is plain text: no suggestion overlay
        // may open while typing a denial reason that starts with `/`, Tab
        // must not complete a command, and Enter must deny with the typed
        // reason instead of running the command.
        let (mut state, _, cmd_tx, mut cmd_rx) = setup();
        let mut mouse_drag = MouseDrag::default();
        let mut clipboard = NoopClipboard;
        let mut pending_run_ids = PendingRunIds::default();

        let mut textarea = TextArea::from(vec!["hello".to_string()]);
        configure_textarea(&mut textarea);

        let tool_id = Uuid::new_v4();
        let effects = presenter::update(
            &mut state,
            TuiEvent::Domain(DomainEvent::ToolPending {
                id: tool_id,
                name: "write".to_string(),
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
            &mut mouse_drag,
            &mut clipboard,
            &mut pending_run_ids,
        )
        .await;
        assert_eq!(state.slash_overlay, SlashOverlay::Hidden);

        // Type "/clear" as the denial reason, mirroring the driver loop:
        // key → effects → per-action recompute.
        for character in "/clear".chars() {
            let effects = presenter::update(
                &mut state,
                TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Char(character)))),
                &textarea_ctx_from(&textarea),
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
            assert_eq!(
                state.slash_overlay,
                SlashOverlay::Hidden,
                "no suggestions while typing a denial reason containing `/`"
            );
        }
        assert_eq!(textarea.lines(), &["/clear"]);

        // Tab must not complete a slash command in the approval input.
        let effects = presenter::update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Tab))),
            &textarea_ctx_from(&textarea),
        );
        assert_eq!(effects, vec![Effect::TextareaKey(key_event(KeyCode::Tab))]);

        // Enter must deny with the typed reason, not run `/clear`.
        let effects = presenter::update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Enter))),
            &textarea_ctx_from(&textarea),
        );
        assert!(
            matches!(
                effects.as_slice(),
                [Effect::DenyTool { id, reason }, Effect::RestoreInputState]
                    if *id == tool_id && reason == "/clear"
            ),
            "Enter must deny with the reason, got {effects:?}"
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
        assert_eq!(
            cmd_rx.try_recv(),
            Ok(AgentCommand::DenyTool {
                id: tool_id,
                reason: "/clear".to_string(),
            })
        );
    }

    #[tokio::test]
    async fn interrupted_model_list_discovery_resumes_after_the_pause() {
        // A result completing while a pause hid the overlay is discarded, so
        // once the pause restores `LoadingModels` the request must be made
        // again for the picker to ever populate.
        let (mut state, _, _cmd_tx, _cmd_rx) = setup();
        let engine: Arc<dyn Engine> = StubEngine::with_models(&["small", "large"], "large");
        let mut pending_model_list: Option<PendingModelList> = None;

        state.slash_overlay = SlashOverlay::LoadingModels;
        resume_interrupted_model_list(&state, &mut pending_model_list, &engine, "/models deep");
        assert!(
            pending_model_list.is_some(),
            "the discarded catalog request must be restarted"
        );

        let (filter, result) = wait_for_model_list(&mut pending_model_list)
            .await
            .expect("the restarted request must produce a result");
        pending_model_list = None;
        apply_model_list_result(&mut state, filter, result, &engine);
        let SlashOverlay::ModelPicker {
            choices, filter, ..
        } = &state.slash_overlay
        else {
            panic!("expected the models picker, got {:?}", state.slash_overlay);
        };
        assert_eq!(filter, "deep");
        assert_eq!(choices.len(), 2);

        // While the original request is still in flight, its result will be
        // applied on arrival — the helper must not start a second one.
        state.slash_overlay = SlashOverlay::LoadingModels;
        start_model_list(&mut pending_model_list, None, &engine);
        resume_interrupted_model_list(&state, &mut pending_model_list, &engine, "/models deep");
        assert_eq!(
            pending_model_list
                .as_ref()
                .map(|pending| pending.filter.clone()),
            Some(None),
            "an in-flight request must not be replaced"
        );
        abort_pending_model_list(&mut pending_model_list);
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
                        reasoning_default: None,
                        input_modalities: None,
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

                    compact_target_ratio: 0.5,
                    soft_compaction_threshold: 0.7,
                    hard_compaction_threshold: 0.9,
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
                        reasoning_default: None,
                        input_modalities: None,
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

                    compact_target_ratio: 0.5,
                    soft_compaction_threshold: 0.7,
                    hard_compaction_threshold: 0.9,
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

                        compact_target_ratio: 0.5,
                        soft_compaction_threshold: 0.7,
                        hard_compaction_threshold: 0.9,
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
    fn suggestion_selection_survives_the_driver_per_action_recompute() {
        // Regression: the Driver recomputes the overlay after every key
        // action (see the key branch of the event loop). Rebuilding the
        // suggestions with `selected: 0` on each recompute made ↑/↓ a no-op
        // in the running app — the presenter moved the highlight and the
        // recompute immediately put it back on the first entry. The unit
        // test above misses this because it skips the recompute.
        let mut state = AppState::new();
        state.refresh_slash_overlay("/");
        let textarea = TextareaContext {
            is_empty: false,
            text: "/".to_string(),
            cursor_row: 0,
            cursor_row_is_last: true,
        };
        let highlighted =
            |state: &AppState| state.selected_slash_command().map(|command| command.name);

        presenter::update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Down))),
            &textarea,
        );
        state.refresh_slash_overlay(&textarea.text);
        assert_eq!(highlighted(&state), Some("/models"));

        presenter::update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Down))),
            &textarea,
        );
        state.refresh_slash_overlay(&textarea.text);
        assert_eq!(highlighted(&state), Some("/tools"));

        presenter::update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Up))),
            &textarea,
        );
        state.refresh_slash_overlay(&textarea.text);
        assert_eq!(highlighted(&state), Some("/models"));
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
    async fn models_lists_an_engine_shortcut_with_its_catalog_reasoning_default() {
        // The catalog's provider-advertised default effort must reach the
        // shortcut row, so its default slot reads `default (medium)` instead
        // of a bare `default`.
        let (mut state, _textarea, cmd_tx, _cmd_rx) = setup();
        let mut stub = StubEngine::with_named_engine(
            &["small", "thinking"],
            "small",
            "openrouter",
            "thinking",
            &["low", "high"],
            None,
            Some(32_000),
            None,
        );
        for model in Arc::get_mut(&mut stub)
            .expect("the stub engine is uniquely owned")
            .models
            .iter_mut()
        {
            if model.id == "thinking" {
                model.reasoning_default = Some("medium".to_string());
            }
        }
        let engine: Arc<dyn Engine> = stub;

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
            shortcut.selection.model.reasoning_default,
            Some("medium".to_string()),
            "the shortcut's model must carry the catalog's advertised default effort"
        );
        assert_eq!(
            shortcut.label,
            "openrouter → stub/thinking  context: 31kb  reasoning: default (medium) ⚙",
            "the shortcut's default slot must name the advertised default"
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
                reasoning_default: None,
                input_modalities: None,
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
