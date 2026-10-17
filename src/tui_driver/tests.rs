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
use std::collections::HashMap;

use crate::llm::engine::{Engine, EngineConfig, EngineImpl, ModelSelection};
use crate::tui_reducer_state::{
    ChatEntry, ConversationView, Focus, InputSnapshot, ModelChoice, ToolCallState,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::models::compaction_switch_notice;
use super::terminal::format_terminal_title;
use crate::tui_reducer_state::InputEvent;

mod effects;
mod input;
mod models;
mod sessions;
mod terminal;
mod updates;

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

#[tokio::test]
async fn clarify_commands_keep_their_destination_after_the_reducer_leaves_pending_mode() {
    let (mut state, _, cmd_tx, mut cmd_rx) = setup();
    let subagent_run_id = Uuid::new_v4();
    let mut pending_run_ids = PendingRunIds::default();
    let mut mouse_drag = MouseDrag::default();
    let mut clipboard = NoopClipboard;

    for run_id in [Some(subagent_run_id), None] {
        for abort in [false, true] {
            let mut textarea = TextArea::from(["unfinished draft"]);
            configure_textarea(&mut textarea);
            let context = textarea_ctx_from(&textarea);
            process_one_update(
                &mut state,
                &mut textarea,
                &cmd_tx,
                &context,
                &UiUpdate::ClarifyPending {
                    id: Uuid::new_v4(),
                    question: "Which option?".to_string(),
                    context: None,
                    choices: None,
                    run_id,
                },
                &mut mouse_drag,
                &mut clipboard,
                &mut pending_run_ids,
            )
            .await;
            assert!(matches!(state.mode, TuiMode::ClarifyPending { .. }));
            assert_eq!(textarea.lines(), &[""]);
            if !abort {
                execute_effects(
                    &mut state,
                    &mut textarea,
                    &cmd_tx,
                    vec![Effect::SetInputText("chosen option".to_string())],
                    &mut mouse_drag,
                    &mut clipboard,
                    &mut pending_run_ids,
                )
                .await;
            }
            let context = textarea_ctx_from(&textarea);
            let effects = presenter::update(
                &mut state,
                TuiEvent::Input(if abort {
                    InputEvent::Escape
                } else {
                    InputEvent::KeyInput(key_event(KeyCode::Enter))
                }),
                &context,
            );
            assert_eq!(state.mode, TuiMode::Pending);
            assert_eq!(
                execute_effects(
                    &mut state,
                    &mut textarea,
                    &cmd_tx,
                    effects,
                    &mut mouse_drag,
                    &mut clipboard,
                    &mut pending_run_ids,
                )
                .await,
                ActionOutcome::Continue,
            );
            let expected_command = if abort {
                AgentCommand::AbortClarify
            } else {
                AgentCommand::AnswerClarify("chosen option".to_string())
            };
            let expected_command = match run_id {
                Some(run_id) => AgentCommand::ForSubagent {
                    run_id,
                    command: Box::new(expected_command),
                },
                None => expected_command,
            };
            assert_eq!(cmd_rx.try_recv(), Ok(expected_command));
            assert_eq!(textarea.lines(), &["unfinished draft"]);
            assert!(state.pending_input_snapshot.is_none());

            execute_effects(
                &mut state,
                &mut textarea,
                &cmd_tx,
                vec![Effect::AbortClarify],
                &mut mouse_drag,
                &mut clipboard,
                &mut pending_run_ids,
            )
            .await;
            assert_eq!(
                cmd_rx.try_recv(),
                Ok(AgentCommand::AbortClarify),
                "resolving a clarify must consume its subagent destination"
            );
            assert!(cmd_rx.try_recv().is_err());
        }
    }
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
            exit_snapshot: None,
            unactivated_clones: Arc::default(),
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
            exit_snapshot: None,
            unactivated_clones: Arc::default(),
        },
    )
    .await
    .map(|(tui_state, _outcome)| tui_state)
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

// ── slash commands ────────────────────────────────────────────────────

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
fn spinner_ticks_while_root_is_idle_but_a_spawned_subagent_is_still_running() {
    // Regression: the root loop's own `TuiMode` can settle to `Idle` while
    // a spawned subagent's `spawn_subagent` tool bubble stays `Running` —
    // the spinner must keep advancing (driving both the footer and the
    // subagent-view header bubble's spinner glyph) rather than freezing.
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
fn spinner_keeps_ticking_while_the_spawned_subagent_itself_is_paused_for_approval() {
    // The subagent's own inner tool-approval pause sets `Focus::ToolApproval`
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
