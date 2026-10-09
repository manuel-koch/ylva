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

use std::sync::Arc;
use std::time::{Duration, Instant};

use ratatui::backend::Backend;
use ratatui::Terminal;
use ratatui_textarea::TextArea;
use tokio::select;
use tokio::sync::mpsc;
use tracing::Instrument;

use crate::agent::{AgentCommand, AgentLoop, AgentMode, Role, UiUpdate};
use crate::agent_factory::AgentFactory;
use crate::app::App;
use crate::config::ProfileConfig;
use crate::events::{spawn_crossterm_bridge, AppAction};
use crate::presenter::{self, TextareaContext, TuiEvent};
use crate::render;
use crate::session_title::{SessionTitleUpdate, TitleAttempt};
use crate::tui_reducer_state::{
    AppState, ConfirmKey, DomainEvent, Effect, SlashOverlay, TuiDisplayMetadata, TuiMode,
    UserMessageState, CONFIRM_TIMEOUT,
};
use crate::tui_render_state::TuiState;

mod effects;
mod input;
mod models;
mod sessions;
mod terminal;
mod updates;

use effects::{execute_effects, execute_slash_effects, split_slash_effects, PendingRunIds};
use input::{app_action_to_tui_event, configure_textarea, scroll_input_at_mouse, MouseDrag};
use models::{
    abort_pending_model_list, apply_model_list_result, resume_interrupted_model_list,
    start_model_list, wait_for_model_list, PendingModelList,
};
use sessions::{picker_mut, selection_error, SessionResult, SessionTasks};
use terminal::{initialize_terminal, install_sigint_relay, set_terminal_title, TerminalTitleGuard};
use updates::{app_state_to_tui_state, prepare_child_replay_effects, process_one_update};

pub use terminal::{ArboardClipboard, ClipboardSink, NoopClipboard};

/// Outcome of processing one event or effect batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionOutcome {
    Continue,
    Quit,
}

/// Interval between spinner frame advances while the agent is busy.
const SPINNER_TICK_INTERVAL: Duration = Duration::from_millis(150);

/// Capacity of the bounded action channel between the crossterm bridge and the
/// async loop, and the per-frame `UiUpdate` drain cap.
const CHANNEL_CAPACITY: usize = 64;

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
    if state.pending_bubble_expansion.is_some() {
        terminal.draw(|frame| {
            render::render_expansion_feedback(
                frame,
                state,
                textarea,
                hint.as_deref(),
                mouse_pos,
                agent.registry().as_ref(),
            );
        })?;
        presenter::update(
            state,
            TuiEvent::Domain(DomainEvent::BubbleExpansionFeedbackShown),
            &TextareaContext {
                is_empty: textarea.lines() == [""],
                text: textarea.lines().join("\n"),
                cursor_row: textarea.cursor().0,
                cursor_row_is_last: textarea.cursor().0 + 1 >= textarea.lines().len().max(1),
            },
        );
    }
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
    exit_snapshot: Option<crate::app::SessionExitHandle>,
    unactivated_clones: Arc<std::sync::Mutex<Vec<uuid::Uuid>>>,
}

impl TuiRuntime {
    fn disabled() -> Self {
        let (_update_tx, update_rx) = mpsc::unbounded_channel();
        Self {
            driver: None,
            update_rx,
            display_metadata: TuiDisplayMetadata::default(),
            input_history_limit: ProfileConfig::default().input_history_limit,
            exit_snapshot: None,
            unactivated_clones: Arc::default(),
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
        exit_snapshot,
        unactivated_clones,
    } = tui_runtime;
    let mut exit_guard = agent.exit_guard(exit_snapshot);
    let mut state = AppState::new();
    state.display_metadata = display_metadata;
    state.enable_render_cache();
    let mut textarea = TextArea::default();
    configure_textarea(&mut textarea);
    let mut replay_store = agent.store();
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
    let mut cmd_tx = agent.command_sender();
    let mut engine = agent.engine();
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
    let mut session_tasks = SessionTasks::new();
    let mut title_updates_open = true;
    let mut active_title_session_id = replay_store.as_ref().map(|store| store.session_id());

    let mut handle = Some(agent.run());

    if let Some(title_driver) = &title_driver {
        title_driver.trigger(&agent);
    }

    let mut spinner_tick = tokio::time::interval(SPINNER_TICK_INTERVAL);
    spinner_tick.tick().await;

    let event_loop_result: anyhow::Result<()> = async {
    if let Some(prompt) = initial_prompt {
        if !prompt.is_empty() {
            state.push_user_bubble(prompt.clone(), UserMessageState::Sent);
            state.mode = TuiMode::Pending;
            cmd_tx.send(AgentCommand::Prompt(prompt)).await?;
        }
    }

    loop {
        replay_store = agent.store();
        draw_frame(terminal, &mut state, &mut textarea, &agent, mouse_pos)?;

        let textarea_ctx = TextareaContext {
            is_empty: textarea.lines() == [""],
            text: textarea.lines().join("\n"),
            cursor_row: textarea.cursor().0,
            cursor_row_is_last: textarea.cursor().0 + 1 >= textarea.lines().len().max(1),
        };

        select! {
            session_result = session_tasks.receiver.recv() => {
                let Some(session_result) = session_result else { continue };
                match session_result {
                    SessionResult::Listed { session_id, picker_id, result } => {
                        if agent.store().map(|store| store.session_id()) != Some(session_id) {
                            continue;
                        }
                        match result {
                            Ok((choices, diagnostics)) => {
                                if let Some(picker) = picker_mut(&mut state, picker_id) {
                                    picker.choices = choices;
                                    picker.phase = crate::tui_reducer_state::SessionPickerPhase::Ready;
                                    if !diagnostics.is_empty() {
                                        picker.error = Some(diagnostics.join("; "));
                                    }
                                    for diagnostic in diagnostics {
                                        tracing::warn!(%diagnostic, "session picker listing diagnostic");
                                    }
                                }
                            }
                            Err(error) => selection_error(&mut state, picker_id, error.to_string()),
                        }
                    }
                    SessionResult::Prepared { session_id, picker_id, created_clone, result } => {
                        let current = agent.store().map(|store| store.session_id()) == Some(session_id);
                        let wanted = current && picker_mut(&mut state, picker_id)
                            .is_some_and(|picker| picker.phase == crate::tui_reducer_state::SessionPickerPhase::Preparing);
                        match result {
                            Ok(mut prepared) if wanted => {
                                abort_pending_model_list(&mut pending_model_list);
                                let shutdown = async {
                                    cmd_tx.send(AgentCommand::Shutdown).await?;
                                    let mut old_handle = handle.take().expect("active runtime");
                                    // Drain old updates while joining so its bounded UI channel cannot deadlock shutdown.
                                    loop {
                                        select! {
                                            context = &mut old_handle => { drop(context??); break; }
                                            Some(_) = ui_rx.recv() => {}
                                        }
                                    }
                                    anyhow::Ok(())
                                }.await;
                                if let Err(error) = shutdown {
                                    if let Some(clone_id) = created_clone {
                                        report_unactivated_clone(&mut state, &unactivated_clones, clone_id);
                                    }
                                    return Err(error);
                                }
                                agent = prepared.agent;
                                ui_rx = prepared.updates;
                                cmd_tx = agent.command_sender();
                                engine = agent.engine();
                                exit_guard.current_store = agent.current_store_handle();
                                let display_metadata = state.display_metadata.clone();
                                state = AppState::new();
                                state.display_metadata = display_metadata;
                                state.enable_render_cache();
                                state.replay_compaction_boundary = prepared.transcript.compaction_transcript_boundary;
                                state.replay_messages(&prepared.transcript.messages, &prepared.child_runs, state.replay_compaction_boundary);
                                state.prepend_input_history(std::mem::take(&mut prepared.input_history));
                                state.engine_display = agent.engine_display_info();
                                state.available_tool_names = agent.registry().names();
                                state.available_tool_names.sort();
                                pending_run_ids = PendingRunIds::default();
                                mouse_drag = MouseDrag::default();
                                input::clear_textarea(&mut textarea);
                                replay_store = agent.store();
                                active_title_session_id = replay_store.as_ref().map(|store| store.session_id());
                                if let Some(store) = replay_store.as_ref() {
                                    let configuration = engine.config();
                                    store.record_model_changed(&configuration.provider_connection, &configuration.model);
                                    if let Err(error) = set_terminal_title(Some(&store.title())) {
                                        tracing::warn!(%error, "could not set activated session title");
                                    }
                                    tracing::info!(session_id = %store.session_id(), cloned = created_clone.is_some(), "session activated");
                                }
                                handle = Some(agent.run());
                                if let Some(driver) = &title_driver { driver.trigger(&agent); }
                            }
                            result => {
                                let newer_preparation = [
                                    Some(&state.slash_overlay),
                                    state.pending_input_snapshot.as_ref().map(|snapshot| &snapshot.slash_overlay),
                                ].into_iter().flatten().any(|overlay| matches!(
                                    overlay, SlashOverlay::SessionPicker(picker)
                                    if picker.phase == crate::tui_reducer_state::SessionPickerPhase::Preparing && picker.identity != picker_id
                                ));
                                if current && !newer_preparation
                                {
                                    if let Err(error) = cmd_tx.try_send(AgentCommand::CancelSessionSwitch) {
                                        tracing::warn!(%error, "could not release session replacement barrier");
                                        state.push_notice(format!("Could not release session replacement barrier: {error}"));
                                    }
                                }
                                if let Err(error) = result {
                                    if wanted { selection_error(&mut state, picker_id, error.to_string()); }
                                }
                                if let Some(clone_id) = created_clone {
                                    report_unactivated_clone(&mut state, &unactivated_clones, clone_id);
                                }
                            }
                        }
                    }
                }
            }
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
                // directly to the textarea; rendering syncs its viewport.
                if let AppAction::MouseInput(mouse) = action {
                    if scroll_input_at_mouse(&state, &mut textarea, mouse) {
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
                    .instrument(agent.session_span())
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
                .instrument(agent.session_span())
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
                    let (session_effects, immediate_slash_effects): (Vec<_>, Vec<_>) =
                        immediate_slash_effects.into_iter().partition(|effect| matches!(
                            effect, Effect::ListSessions | Effect::SelectSession { .. } | Effect::CancelSessionSelection
                        ));
                    for effect in session_effects {
                        match effect {
                            Effect::ListSessions => session_tasks.list(&mut state, agent.store()),
                            Effect::SelectSession { source_id, action } => {
                                session_tasks.prepare(&mut state, source_id, action, &agent, title_driver.as_ref());
                            }
                            Effect::CancelSessionSelection => {
                                session_tasks.cancel();
                                if let Err(error) = cmd_tx.try_send(AgentCommand::CancelSessionSwitch) {
                                    tracing::warn!(%error, "could not cancel session replacement");
                                    state.push_notice(format!("Could not cancel session replacement: {error}"));
                                }
                            }
                            _ => {}
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
                .instrument(agent.session_span())
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
                .instrument(agent.session_span())
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
                            .instrument(agent.session_span())
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
                .instrument(agent.session_span())
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
                .instrument(agent.session_span())
                .await;
            }

            _ = spinner_tick.tick() => {
                if spinner_should_tick(&state) {
                    state.spinner_frame = state.spinner_frame.wrapping_add(1);
                }
            }
        }
    }

    Ok(())
    }.await;

    abort_pending_model_list(&mut pending_model_list);
    {
        let finishing = session_tasks.finish();
        tokio::pin!(finishing);
        loop {
            select! {
                () = &mut finishing => break,
                Some(_) = ui_rx.recv() => {}
            }
        }
    }
    while let Ok(result) = session_tasks.receiver.try_recv() {
        if let SessionResult::Prepared {
            created_clone: Some(clone_id),
            ..
        } = result
        {
            report_unactivated_clone(&mut state, &unactivated_clones, clone_id);
        }
    }
    drop(cmd_tx);
    drop(agent);
    if let Some(mut handle) = handle {
        loop {
            select! {
                result = &mut handle => { result??; break; }
                Some(_) = ui_rx.recv() => {}
            }
        }
    }
    event_loop_result?;
    Ok(app_state_to_tui_state(state))
}

fn report_unactivated_clone(
    state: &mut AppState,
    unactivated_clones: &Arc<std::sync::Mutex<Vec<uuid::Uuid>>>,
    clone_id: uuid::Uuid,
) {
    tracing::warn!(%clone_id, "clone created but not activated");
    state.push_notice(format!("Clone created but not activated: {clone_id}"));
    unactivated_clones
        .lock()
        .expect("clone notice mutex")
        .push(clone_id);
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
    exit_snapshot: Option<crate::app::SessionExitHandle>,
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
            exit_snapshot: None,
        }
    }

    /// Set the active profile and workspace paths shown by the TUI.
    pub fn with_display_metadata(mut self, display_metadata: TuiDisplayMetadata) -> Self {
        self.display_metadata = display_metadata;
        self
    }

    pub fn with_exit_snapshot(mut self, snapshot: crate::app::SessionExitHandle) -> Self {
        self.exit_snapshot = Some(snapshot);
        self
    }
}

impl App for TuiApp {
    async fn run(self) -> anyhow::Result<u8> {
        let _terminal_title_guard = TerminalTitleGuard::new()?;
        let (_guard, mut terminal) = initialize_terminal()?;

        let (action_tx, action_rx) = mpsc::channel::<AppAction>(CHANNEL_CAPACITY);
        spawn_crossterm_bridge(action_tx.clone());
        install_sigint_relay(action_tx)?;

        let (agent, ui_rx) = self
            .factory
            .create(&self.profile, &self.agent_type, Role::Root)?;
        let configuration_factory = Arc::new(self.factory.without_store());
        drop(self.factory);
        let initial_session_title = agent.store().map(|store| store.title());
        set_terminal_title(initial_session_title.as_deref())?;
        let (title_update_tx, title_update_rx) = mpsc::unbounded_channel();
        let title_driver = Some(TitleDriver {
            factory: configuration_factory,
            profile: self.profile.clone(),
            attempt: TitleAttempt::default(),
            title_update_tx,
        });

        let unactivated_clones = Arc::new(std::sync::Mutex::new(Vec::new()));
        let result = run_tui_with_title(
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
                exit_snapshot: self.exit_snapshot,
                unactivated_clones: unactivated_clones.clone(),
            },
        )
        .await;
        drop(terminal);
        drop(_guard);
        drop(_terminal_title_guard);
        for clone_id in unactivated_clones
            .lock()
            .expect("clone notice mutex")
            .iter()
        {
            eprintln!("Clone created but not activated: {clone_id}");
        }
        result.map(|_| 0u8)
    }
}

#[cfg(test)]
mod tests;
