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

//! The TUI event loop: multiplexes terminal actions, agent updates, timers
//! and session switches.

use std::sync::Arc;

use ratatui::backend::Backend;
use ratatui::Terminal;
use ratatui_textarea::TextArea;
use tokio::select;
use tokio::sync::mpsc;
use tracing::Instrument;

use super::effects::{execute_effects, execute_slash_effects, split_slash_effects, PendingRunIds};
use super::frame::draw_frame;
use super::input::{app_action_to_tui_event, configure_textarea, scroll_input_at_mouse, MouseDrag};
use super::models::{
    abort_pending_model_list, apply_model_list_result, resume_interrupted_model_list,
    start_model_list, wait_for_model_list, PendingModelList,
};
use super::runtime::TuiRuntime;
use super::sessions::{picker_mut, selection_error, SessionResult, SessionTasks};
use super::terminal::set_terminal_title;
use super::timers::{bubble_click_timeout, confirm_timeout, spinner_should_tick};
use super::title::trigger_title_on_stream_done;
use super::updates::{app_state_to_tui_state, prepare_subagent_replay_effects, process_one_update};
use super::{ActionOutcome, ClipboardSink, CHANNEL_CAPACITY, SPINNER_TICK_INTERVAL};
use crate::agent::{AgentCommand, AgentLoop, UiUpdate};
use crate::events::AppAction;
use crate::presenter::{self, TextareaContext, TuiEvent};
use crate::run_outcome::RunOutcome;
use crate::tui_reducer_state::{
    AppState, DomainEvent, Effect, SlashOverlay, TuiMode, UserMessageState,
};
use crate::tui_render_state::TuiState;

pub(super) async fn run_tui_with_title<B>(
    terminal: &mut Terminal<B>,
    agent: AgentLoop,
    ui_rx: mpsc::Receiver<UiUpdate>,
    initial_prompt: Option<String>,
    mut action_rx: mpsc::Receiver<AppAction>,
    clipboard: &mut dyn ClipboardSink,
    tui_runtime: TuiRuntime,
) -> anyhow::Result<(TuiState, RunOutcome)>
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
        startup_notice,
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
        let subagent_runs = match crate::store::Store::read_subagent_runs(store.path()) {
            Ok(subagent_runs) => subagent_runs,
            Err(error) => {
                tracing::warn!(
                    %error,
                    path = %store.path().display(),
                    "could not replay subagent-run lifecycle records"
                );
                Vec::new()
            }
        };
        state.replay_messages(&messages, &subagent_runs, state.replay_compaction_boundary);
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
    if let Some(notice) = startup_notice {
        state.push_notice_to(None, notice);
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
                                state.replay_messages(&prepared.transcript.messages, &prepared.subagent_runs, state.replay_compaction_boundary);
                                state.prepend_input_history(std::mem::take(&mut prepared.input_history));
                                state.engine_display = agent.engine_display_info();
                                state.available_tool_names = agent.registry().names();
                                state.available_tool_names.sort();
                                pending_run_ids = PendingRunIds::default();
                                mouse_drag = MouseDrag::default();
                                super::input::clear_textarea(&mut textarea);
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
                        prepare_subagent_replay_effects(&mut state, effects, replay_store.as_ref());
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

                // Ctrl+C/Esc do not come through here: only an external signal
                // interrupts the run, whatever the TUI is showing.
                if matches!(action, AppAction::StopSignal) {
                    pending_run_ids.record_stop_signal();
                }
                let Some(event) = app_action_to_tui_event(action) else { continue };
                let was_loading_models = matches!(state.slash_overlay, SlashOverlay::LoadingModels);
                let effects = presenter::update(&mut state, event, &textarea_ctx);
                // Slash-command effects are the only ones needing the engine,
                // and only key input can produce them.
                let (slash_effects, effects) = split_slash_effects(effects);
                let effects =
                    prepare_subagent_replay_effects(&mut state, effects, replay_store.as_ref());
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
                    prepare_subagent_replay_effects(&mut state, effects, replay_store.as_ref());
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
                    prepare_subagent_replay_effects(&mut state, effects, replay_store.as_ref());
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
    // Updates drained during teardown are not recorded in the run outcome:
    // closing the agent cancels its streams, which is not a user interruption.
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
    Ok((app_state_to_tui_state(state), pending_run_ids.run_outcome))
}

pub(super) fn report_unactivated_clone(
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
