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

#[test]
fn session_picker_renders_actions_timestamps_and_elided_titles_on_small_terminals() {
    use ylva::tui_reducer_state::{
        SessionAction, SessionChoice, SessionPicker, SessionPickerPhase, SlashOverlay,
    };
    for (width, height) in [(120, 40), (60, 15), (30, 9), (10, 6)] {
        let mut state = ylva::AppState::new();
        let mut picker = SessionPicker::loading();
        picker.phase = SessionPickerPhase::Ready;
        picker.action = SessionAction::Clone;
        picker.choices = vec![SessionChoice {
            session_id: uuid::Uuid::new_v4(),
            title: "Long safe 日本語 title ".repeat(20),
            last_used_at: "2026-01-01 12:00:00 +01:00".into(),
        }];
        state.slash_overlay = SlashOverlay::SessionPicker(picker);
        let buffer = render_offline(&mut state, width, height);
        let text = rendered_text(&buffer);
        if width >= 30 {
            assert_eq!(text.matches("Resume").count(), 1, "{text}");
            assert_eq!(text.matches("Clone").count(), 1, "{text}");
            assert!(!text.contains("[x]"), "{text}");
            assert!(!text.contains("[ ]"), "{text}");
            assert!(text.contains("2026-01-01"), "{text}");
            let layout = state.layout.slash_overlay;
            assert_eq!(layout.resume_action.width, " Resume ".len() as u16);
            assert_eq!(layout.clone_action.width, " Clone ".len() as u16);
            assert_ne!(
                buffer[(layout.resume_action.x, layout.resume_action.y)].bg,
                buffer[(layout.clone_action.x, layout.clone_action.y)].bg
            );
            let popup_rows = text
                .lines()
                .skip(usize::from(layout.content_area.y.saturating_sub(1)))
                .take(usize::from(
                    layout.clone_action.y.saturating_sub(layout.content_area.y) + 3,
                ))
                .collect::<Vec<_>>();
            assert!(popup_rows[0].contains("Sessions"), "{text}");
            assert!(!popup_rows[0].contains("Resume"), "{text}");
            assert!(
                popup_rows
                    .iter()
                    .all(|row| !row.contains("enter") && !row.contains("esc")),
                "{text}"
            );
        }
        if width >= 60 {
            assert!(text.contains("+01:00"), "{text}");
            assert!(text.contains('…'), "{text}");
            assert_eq!(text.matches("enter confirm").count(), 1, "{text}");
            assert_eq!(text.matches("esc cancel").count(), 1, "{text}");
        }
    }
}

#[test]
fn session_picker_buttons_use_rendered_hit_targets_without_activating() {
    use ylva::tui_reducer_state::{
        Effect, InputEvent, SessionAction, SessionChoice, SessionPicker, SessionPickerPhase,
        SlashOverlay,
    };
    let textarea = ylva::TextareaContext {
        is_empty: true,
        text: String::new(),
        cursor_row: 0,
        cursor_row_is_last: true,
    };
    for (width, height) in [(120, 40), (60, 15), (30, 9)] {
        let mut state = ylva::AppState::new();
        let mut picker = SessionPicker::loading();
        picker.phase = SessionPickerPhase::Ready;
        let session_id = uuid::Uuid::new_v4();
        picker.choices.push(SessionChoice {
            session_id,
            title: "target".into(),
            last_used_at: "date".into(),
        });
        state.slash_overlay = SlashOverlay::SessionPicker(picker);
        for action in [SessionAction::Clone, SessionAction::Resume] {
            render_offline(&mut state, width, height);
            let layout = state.layout.slash_overlay.clone();
            let button = match action {
                SessionAction::Resume => layout.resume_action,
                SessionAction::Clone => layout.clone_action,
            };
            assert!(button.width > 0);
            let effects = ylva::presenter::update(
                &mut state,
                ylva::TuiEvent::Mouse(crossterm::event::MouseEvent {
                    kind: crossterm::event::MouseEventKind::Down(
                        crossterm::event::MouseButton::Left,
                    ),
                    column: button.x,
                    row: button.y,
                    modifiers: KeyModifiers::NONE,
                }),
                &textarea,
            );
            assert!(effects.is_empty());
            let SlashOverlay::SessionPicker(picker) = &state.slash_overlay else {
                panic!()
            };
            assert_eq!(picker.action, action);
            assert_eq!(
                ylva::presenter::update(
                    &mut state,
                    ylva::TuiEvent::Input(InputEvent::KeyInput(KeyEvent::new(
                        KeyCode::Enter,
                        KeyModifiers::NONE
                    ))),
                    &textarea
                ),
                vec![Effect::SelectSession {
                    source_id: session_id,
                    action
                }]
            );
        }
    }
}

#[test]
fn session_picker_status_and_errors_are_shown_once_without_popup_keybindings() {
    use ylva::tui_reducer_state::{SessionChoice, SessionPicker, SessionPickerPhase, SlashOverlay};
    for (phase, empty, error, expected) in [
        (
            SessionPickerPhase::Loading,
            true,
            None,
            "Loading sessions...",
        ),
        (SessionPickerPhase::Preparing, false, None, "Resuming..."),
        (
            SessionPickerPhase::Ready,
            true,
            None,
            "No other root sessions.",
        ),
        (
            SessionPickerPhase::Ready,
            false,
            Some("Selection failed"),
            "Selection failed",
        ),
    ] {
        for (width, height) in [(120, 40), (60, 15), (30, 9)] {
            let mut state = ylva::AppState::new();
            let mut picker = SessionPicker::loading();
            picker.phase = phase;
            picker.error = error.map(str::to_string);
            if !empty {
                picker.choices.push(SessionChoice {
                    session_id: uuid::Uuid::new_v4(),
                    title: "target".into(),
                    last_used_at: "date".into(),
                });
            }
            state.slash_overlay = SlashOverlay::SessionPicker(picker);
            let text = rendered_text(&render_offline(&mut state, width, height));
            assert_eq!(text.matches(expected).count(), 1, "{text}");
            assert!(
                !text.contains("Esc cancels") && !text.contains("Enter confirms"),
                "{text}"
            );
            assert!(!text.contains("loading models"), "{text}");
        }
    }
}

#[tokio::test]
async fn typed_manual_compaction_preserves_persisted_transcript_and_replays_marker() {
    let directory = tempfile::tempdir().unwrap();
    let store = ylva::Store::create(
        directory.path(),
        directory.path(),
        uuid::Uuid::new_v4(),
        "mock",
        "test",
        Some("default"),
    )
    .unwrap();
    let (backend, last_buffer, _) = InspectableTestBackend::new(140, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    let conversation = Arc::new(EngineImpl::new(
        Arc::new(MockProvider::new(
            vec!["completed answer".into()],
            Duration::ZERO,
        )),
        common::test_engine_config(),
    ));
    let summary = Arc::new(EngineImpl::new(
        Arc::new(MockProvider::new(
            vec!["Short factual summary.".into()],
            Duration::ZERO,
        )),
        common::test_engine_config(),
    ));
    let (agent, updates) = AgentLoop::new(conversation, Arc::new(ToolRegistry::new()));
    let agent = agent
        .with_compaction_engine(summary)
        .with_store(store.clone());
    let (actions, action_rx) = mpsc::channel(64);
    let run = tokio::spawn(async move {
        tui_driver::run_tui(
            &mut terminal,
            agent,
            updates,
            None,
            action_rx,
            &mut NoopClipboard,
        )
        .await
        .unwrap()
    });
    type_and_enter(&actions, &"Older useful fact. ".repeat(30)).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            store.flush().await;
            if ylva::Store::read_messages(store.path()).unwrap().len() == 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    type_and_enter(&actions, "Latest explicit task must remain verbatim").await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            store.flush().await;
            if ylva::Store::read_messages(store.path()).unwrap().len() == 4 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let original = ylva::Store::read_messages(store.path()).unwrap();
    type_and_enter(&actions, "/compact").await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let text = rendered_text(&last_buffer.lock().unwrap());
            if text.contains("messages summarized") {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("manual compaction marker should render");
    drop(actions);
    let state = run.await.unwrap();
    store.flush().await;
    let persisted = ylva::Store::read_session_state(store.path()).unwrap();
    assert_eq!(persisted.messages, original);
    assert_eq!(persisted.compaction_transcript_boundary, Some(2));
    assert_eq!(
        persisted
            .events
            .iter()
            .filter(|event| matches!(event, ylva::StoreEvent::Compaction { .. }))
            .count(),
        1
    );
    assert!(state.chat_entries.iter().any(|entry| matches!(
        entry,
        ChatEntry::CompactionMarker {
            summarized_messages: 2
        }
    )));
    let mut replay = ylva::AppState::new();
    replay.replay_messages(
        &persisted.messages,
        &[],
        persisted.compaction_transcript_boundary,
    );
    assert!(replay.chat_entries.iter().any(|entry| matches!(
        entry,
        ChatEntry::CompactionMarker {
            summarized_messages: 2
        }
    )));
}

#[tokio::test]
async fn compaction_capacity_warning_is_rendered_at_startup_before_any_prompt() {
    let (backend, last_buffer, _) = InspectableTestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    let (action_tx, action_rx) = mpsc::channel(64);
    let mut conversation_config = common::test_engine_config();
    conversation_config.context_length = Some(98_304);
    let mut summary_config = common::test_engine_config();
    summary_config.context_length = Some(32_768);
    summary_config.model = "small-summarizer".to_string();
    let conversation_engine = Arc::new(EngineImpl::new(
        Arc::new(MockProvider::new(
            vec!["the answer".to_string()],
            Duration::ZERO,
        )),
        conversation_config,
    ));
    let summary_engine = Arc::new(EngineImpl::new(
        Arc::new(MockProvider::new(Vec::new(), Duration::ZERO)),
        summary_config,
    ));
    let (agent, ui_rx) = AgentLoop::new(conversation_engine, Arc::new(ToolRegistry::new()));
    let agent = agent.with_compaction_engine(summary_engine);
    let run_handle = tokio::spawn(async move {
        tui_driver::run_tui(
            &mut terminal,
            agent,
            ui_rx,
            None,
            action_rx,
            &mut NoopClipboard,
        )
        .await
        .expect("run_tui completed")
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let rendered = rendered_text(&last_buffer.lock().expect("buffer snapshot lock"));
            if rendered.contains("small-summarizer")
                && rendered.contains("Warning: conversation engine")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("capacity warning must render at startup without a user prompt");
    type_and_enter(&action_tx, "question").await;
    let rendered = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let rendered = rendered_text(&last_buffer.lock().expect("buffer snapshot lock"));
            if rendered.contains("the answer") {
                break rendered;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("warning and response should render");
    assert!(rendered.contains("small-summarizer"), "{rendered}");
    assert!(rendered.contains("32768"), "{rendered}");
    assert!(rendered.contains("68812"), "{rendered}");
    assert_eq!(
        rendered.matches("Warning: conversation engine").count(),
        1,
        "{rendered}"
    );
    assert!(
        rendered
            .find("Warning: conversation engine")
            .expect("capacity warning")
            < rendered.find("the answer").expect("response"),
        "{rendered}",
    );
    drop(action_tx);
    run_handle.await.expect("run task joined");
}

#[tokio::test]
async fn switching_models_rechecks_compaction_capacity_before_any_prompt() {
    for switch_provider_connection in [false, true] {
        let (backend, last_buffer, _) = InspectableTestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let (action_tx, action_rx) = mpsc::channel(64);
        let mut safe_config = common::test_engine_config();
        safe_config.provider_connection = "safe-provider".to_string();
        safe_config.model = "safe-conversation".to_string();
        safe_config.context_length = None;
        let mut large_config = safe_config.clone();
        if switch_provider_connection {
            large_config.provider_connection = "large-provider".to_string();
        }
        large_config.model = "large-conversation".to_string();
        let mut summary_config = common::test_engine_config();
        summary_config.model = "small-summarizer".to_string();
        summary_config.context_length = Some(32_768);
        let large_model = ModelInfo {
            id: "large-conversation".to_string(),
            context_length: Some(98_304),
            reasoning_efforts: None,
            reasoning_default: None,
            input_modalities: None,
        };
        let safe_engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
            Arc::new(CatalogMockProvider {
                provider: MockProvider::new(vec!["the answer".to_string()], Duration::ZERO),
                models: vec![
                    ModelInfo {
                        id: "safe-conversation".to_string(),
                        context_length: Some(32_768),
                        ..large_model.clone()
                    },
                    large_model.clone(),
                ],
            }),
            safe_config,
        ));
        let large_engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
            Arc::new(CatalogMockProvider {
                provider: MockProvider::new(vec!["the answer".to_string()], Duration::ZERO),
                models: vec![large_model],
            }),
            large_config,
        ));
        let conversation_engine = Arc::new(
            ylva::EngineRouter::new(
                std::collections::HashMap::from([
                    ("safe".to_string(), safe_engine),
                    ("large".to_string(), large_engine),
                ]),
                "safe",
            )
            .expect("test router"),
        );
        let summary_engine = Arc::new(EngineImpl::new(
            Arc::new(MockProvider::new(Vec::new(), Duration::ZERO)),
            summary_config,
        ));
        let (agent, ui_rx) = AgentLoop::new(conversation_engine, Arc::new(ToolRegistry::new()));
        let agent = agent.with_compaction_engine(summary_engine);
        let run_handle = tokio::spawn(async move {
            tui_driver::run_tui(
                &mut terminal,
                agent,
                ui_rx,
                None,
                action_rx,
                &mut NoopClipboard,
            )
            .await
            .expect("run_tui completed")
        });
        type_and_enter(&action_tx, "/models large").await;
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let rendered = rendered_text(&last_buffer.lock().expect("buffer snapshot lock"));
                if rendered.contains("large-conversation") {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the large model appears in the picker");
        // First Enter opens the parameter popup; the second confirms the switch.
        type_and_enter(&action_tx, "").await;
        type_and_enter(&action_tx, "").await;
        let rendered = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let rendered = rendered_text(&last_buffer.lock().expect("buffer snapshot lock"));
                if rendered.contains("Warning: conversation engine")
                    && rendered.contains("small-summarizer")
                {
                    break rendered;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "switching must warn while idle, without sending a conversation prompt:\n{}",
                rendered_text(&last_buffer.lock().expect("buffer snapshot lock")),
            )
        });
        assert!(rendered.contains("98304"), "{rendered}");
        assert!(rendered.contains("32768"), "{rendered}");
        assert!(rendered.contains("Idle"), "{rendered}");
        assert!(!rendered.contains("the answer"), "{rendered}");
        type_and_enter(&action_tx, "question").await;
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let rendered = rendered_text(&last_buffer.lock().expect("buffer snapshot lock"));
                if rendered.contains("the answer") {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the first prompt completes");
        drop(action_tx);
        let state = run_handle.await.expect("run task joined");
        assert_eq!(
            state
                .chat_entries
                .iter()
                .filter(|entry| matches!(
                    entry, ChatEntry::Notice { text } if text.contains("Warning: conversation engine")
                ))
                .count(),
            1,
            "the first request must not repeat the model-switch warning: {:?}",
            state.chat_entries,
        );
    }
}

#[tokio::test]
async fn model_switch_capacity_checks_keep_controls_responsive_and_replace_pending_checks() {
    let catalog_started = Arc::new(Notify::new());
    let release_catalog = Arc::new(Notify::new());
    let mut engines: std::collections::HashMap<String, Arc<dyn Engine>> =
        std::collections::HashMap::new();
    for (engine_name, context_length) in [
        ("safe", Some(32_768)),
        ("delayed", None),
        ("latest", Some(98_304)),
    ] {
        let mut engine =
            DelayedCatalogEngine::new(catalog_started.clone(), release_catalog.clone());
        engine.config.provider_connection = format!("{engine_name}-provider");
        engine.config.model = format!("{engine_name}-model");
        engine.config.context_length = context_length;
        if context_length.is_none() {
            engine = engine.with_catalog_hydration();
        }
        engines.insert(engine_name.to_string(), Arc::new(engine));
    }
    let summary_engine = engines["safe"].clone();
    let conversation_engine =
        Arc::new(ylva::EngineRouter::new(engines, "safe").expect("test router"));
    let (mut agent, mut updates) =
        AgentLoop::new(conversation_engine.clone(), Arc::new(ToolRegistry::new()));
    agent = agent.with_compaction_engine(summary_engine);
    let handle = agent.run();
    agent
        .send_command(ylva::AgentCommand::Stop {
            reason: "startup barrier".to_string(),
        })
        .await;
    let idle = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match updates.recv().await {
                Some(ylva::UiUpdate::ContextUsage { .. }) => continue,
                other => break other,
            }
        }
    })
    .await;
    assert!(matches!(idle, Ok(Some(ylva::UiUpdate::AgentIdle))));
    for engine_name in ["delayed", "latest"] {
        let provider_connection = format!("{engine_name}-provider");
        let model = format!("{engine_name}-model");
        conversation_engine
            .switch_model_option(&ylva::ProviderModelOption {
                context_length_override: None,
                provider_connection: provider_connection.clone(),
                model: ModelInfo {
                    id: model.clone(),
                    context_length: None,
                    reasoning_efforts: None,
                    reasoning_default: None,
                    input_modalities: None,
                },
                reasoning_effort: None,
            })
            .expect("switch to the requested provider");
        agent
            .send_command(ylva::AgentCommand::RecordModelChanged {
                provider_connection,
                model,
            })
            .await;
        if engine_name == "delayed" {
            tokio::time::timeout(Duration::from_secs(1), catalog_started.notified())
                .await
                .expect("model-switch capacity check resolves missing metadata");
            agent
                .send_command(ylva::AgentCommand::Stop {
                    reason: "controls must remain responsive".to_string(),
                })
                .await;
            let idle = tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    match updates.recv().await {
                        Some(ylva::UiUpdate::ContextUsage { .. }) => continue,
                        other => break other,
                    }
                }
            })
            .await;
            assert!(matches!(idle, Ok(Some(ylva::UiUpdate::AgentIdle))));
        }
    }
    let update = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match updates.recv().await {
                Some(ylva::UiUpdate::ContextUsage { .. }) => continue,
                other => break other,
            }
        }
    })
    .await
    .expect("the latest switch must replace the blocked check")
    .expect("capacity warning");
    assert!(matches!(
        update,
        ylva::UiUpdate::Notice { text, run_id: None }
            if text.contains("latest-provider/latest-model")
                && text.contains("98304")
                && text.contains("32768")
                && !text.contains("delayed-model")
    ));
    drop(agent);
    handle.await.expect("loop joins").expect("loop shuts down");
    release_catalog.notify_one();
    assert!(
        updates.try_recv().is_err(),
        "no stale switch notice is emitted"
    );
}

#[tokio::test]
async fn startup_catalog_checks_keep_the_tui_responsive_and_cancel_a_waiting_prompt() {
    let catalog_started = Arc::new(Notify::new());
    let release_catalog = Arc::new(Notify::new());
    let chat_started = Arc::new(Notify::new());
    let release_chat = Arc::new(Notify::new());
    let engine = Arc::new(
        DelayedCatalogEngine::with_chat(
            catalog_started.clone(),
            release_catalog.clone(),
            chat_started.clone(),
            release_chat.clone(),
        )
        .with_catalog_hydration(),
    );
    let (backend, last_buffer, _) = InspectableTestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    let (action_tx, action_rx) = mpsc::channel(64);
    let (agent, ui_rx) = AgentLoop::new(engine.clone(), Arc::new(ToolRegistry::new()));
    let agent = agent.with_compaction_engine(engine.clone());
    let run_handle = tokio::spawn(async move {
        tui_driver::run_tui(
            &mut terminal,
            agent,
            ui_rx,
            None,
            action_rx,
            &mut NoopClipboard,
        )
        .await
        .expect("run_tui completed")
    });
    tokio::time::timeout(Duration::from_secs(1), catalog_started.notified())
        .await
        .expect("startup resolves an unconfigured context window");
    type_and_enter(&action_tx, "cancel this waiting prompt").await;
    send(&action_tx, AppAction::Escape).await;
    send(&action_tx, AppAction::Escape).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let rendered = rendered_text(&last_buffer.lock().expect("buffer snapshot lock"));
            if rendered.contains("cancel this waiting prompt") && rendered.contains("Idle") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("input and cancellation must work while startup metadata is pending");
    release_catalog.notify_one();
    type_and_enter(&action_tx, "new question").await;
    tokio::time::timeout(Duration::from_secs(1), chat_started.notified())
        .await
        .expect("a new prompt starts normally after cancelling the waiting prompt");
    release_chat.notify_one();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let rendered = rendered_text(&last_buffer.lock().expect("buffer snapshot lock"));
            if rendered.contains("conversation update") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the replacement prompt completes");
    drop(action_tx);
    let state = run_handle.await.expect("run task joined");
    assert_eq!(state.agent_mode, ylva::AgentMode::Idle);
    assert_eq!(
        engine.chat_calls.load(Ordering::SeqCst),
        1,
        "the cancelled waiting prompt must never reach the provider",
    );
}

#[tokio::test]
async fn models_command_renders_loading_feedback_before_catalog_finishes() {
    let catalog_started = Arc::new(Notify::new());
    let release_catalog = Arc::new(Notify::new());
    let engine = Arc::new(DelayedCatalogEngine::new(
        Arc::clone(&catalog_started),
        Arc::clone(&release_catalog),
    ));
    let registry = Arc::new(ToolRegistry::new());
    let (backend, last_buffer, _) = InspectableTestBackend::new(80, 24);
    let terminal = Terminal::new(backend).expect("test terminal");
    let (action_tx, action_rx) = mpsc::channel(64);

    let run_handle = tokio::spawn({
        let mut terminal = terminal;
        async move {
            let (agent, ui_rx) = AgentLoop::new(engine, registry);
            tui_driver::run_tui(
                &mut terminal,
                agent,
                ui_rx,
                None,
                action_rx,
                &mut NoopClipboard,
            )
            .await
            .expect("run_tui completed")
        }
    });

    let catalog_started_wait = catalog_started.notified();
    type_and_enter(&action_tx, "/models").await;
    tokio::time::timeout(Duration::from_secs(1), catalog_started_wait)
        .await
        .expect("model catalog request should start");

    let rendered = last_buffer.lock().expect("buffer snapshot lock").clone();
    let rendered_text = rendered_text(&rendered);
    assert!(
        rendered_text.contains("Loading models"),
        "the TUI should show loading feedback while the provider catalog is pending:\n{rendered_text}"
    );

    release_catalog.notify_one();
    drop(action_tx);
    run_handle.await.expect("run task joined");
}

#[tokio::test]
async fn model_catalog_does_not_block_conversation_updates() {
    let catalog_started = Arc::new(Notify::new());
    let release_catalog = Arc::new(Notify::new());
    let chat_started = Arc::new(Notify::new());
    let release_chat = Arc::new(Notify::new());
    let engine = Arc::new(DelayedCatalogEngine::with_chat(
        Arc::clone(&catalog_started),
        Arc::clone(&release_catalog),
        Arc::clone(&chat_started),
        Arc::clone(&release_chat),
    ));
    let registry = Arc::new(ToolRegistry::new());
    let (backend, last_buffer, frame_count) = InspectableTestBackend::new(80, 24);
    let terminal = Terminal::new(backend).expect("test terminal");
    let (action_tx, action_rx) = mpsc::channel(64);

    let run_handle = tokio::spawn({
        let mut terminal = terminal;
        async move {
            let (agent, ui_rx) = AgentLoop::new(engine, registry);
            tui_driver::run_tui(
                &mut terminal,
                agent,
                ui_rx,
                None,
                action_rx,
                &mut NoopClipboard,
            )
            .await
            .expect("run_tui completed")
        }
    });

    type_and_enter(&action_tx, "hi").await;
    tokio::time::timeout(Duration::from_secs(1), chat_started.notified())
        .await
        .expect("conversation request should start");

    type_and_enter(&action_tx, "/models").await;
    tokio::time::timeout(Duration::from_secs(1), catalog_started.notified())
        .await
        .expect("model catalog request should start");

    let frames_before_conversation = frame_count.load(Ordering::SeqCst);
    release_chat.notify_one();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let rendered = last_buffer.lock().expect("buffer snapshot lock").clone();
            if rendered_text(&rendered).contains("conversation update") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("conversation updates should render while model discovery is pending");
    assert!(
        frame_count.load(Ordering::SeqCst) > frames_before_conversation,
        "the driver should continue rendering while the catalog is pending"
    );

    release_catalog.notify_one();
    drop(action_tx);
    run_handle.await.expect("run task joined");
}

#[test]
fn help_popup_footer_shows_popup_controls_instead_of_input_controls() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::TuiState;

    let mut terminal = Terminal::new(TestBackend::new(120, 20)).expect("terminal");
    let mut state = TuiState::new();
    state.help_popup.show_help = true;
    let mut textarea = TextArea::default();

    terminal
        .draw(|frame| {
            tui_widgets::draw(
                frame,
                &mut state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            );
        })
        .expect("draw");

    let rendered: String = (0..20u16)
        .flat_map(|row| (0..120u16).map(move |column| (column, row)))
        .map(|(column, row)| {
            terminal
                .backend()
                .buffer()
                .cell((column, row))
                .unwrap()
                .symbol()
                .to_string()
        })
        .collect();
    assert!(
        rendered.contains("esc close · ctrl-c quit · F1 close"),
        "help popup footer should advertise popup controls: {rendered:?}"
    );
    assert!(
        !rendered.contains("enter send"),
        "help popup footer must not advertise sending: {rendered:?}"
    );
    assert!(
        !rendered.contains("esc clear draft"),
        "help popup footer must not advertise clearing the draft: {rendered:?}"
    );
    assert!(
        !rendered.contains("scroll"),
        "help popup footer must not advertise cursor or scroll controls: {rendered:?}"
    );
}

#[test]
fn busy_help_popup_footer_shows_cancel_and_quit() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::{AgentMode, TuiState};

    let mut terminal = Terminal::new(TestBackend::new(120, 20)).expect("terminal");
    let mut state = TuiState::new();
    state.help_popup.show_help = true;
    state.parent_agent_mode = AgentMode::Streaming;
    let mut textarea = TextArea::default();

    terminal
        .draw(|frame| {
            tui_widgets::draw(
                frame,
                &mut state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            );
        })
        .expect("draw");

    let rendered: String = (0..20u16)
        .flat_map(|row| (0..120u16).map(move |column| (column, row)))
        .map(|(column, row)| {
            terminal
                .backend()
                .buffer()
                .cell((column, row))
                .unwrap()
                .symbol()
                .to_string()
        })
        .collect();
    assert!(
        rendered.contains("esc close · ctrl-c cancel & quit · F1 close"),
        "busy help popup footer should advertise cancellation and quit: {rendered:?}"
    );
}

#[test]
fn model_picker_footer_shows_picker_controls_instead_of_send() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_reducer_state::{ModelChoice, SlashOverlay};
    use ylva::tui_widgets;
    use ylva::{ModelInfo, ProviderModelOption, TuiState};

    let backend = TestBackend::new(80, 20);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut state = TuiState {
        slash_overlay: SlashOverlay::ModelPicker {
            parameters: None,
            choices: vec![ModelChoice {
                catalog_context_length: None,
                label: ylva::tui_reducer_state::ChoiceLabel {
                    identity: "stub/thinking".to_string(),
                    details: vec!["reasoning: default".to_string()],
                },
                initial_reasoning_effort: None,
                selection: ProviderModelOption {
                    context_length_override: None,
                    provider_connection: "stub".to_string(),
                    model: ModelInfo {
                        id: "thinking".to_string(),
                        context_length: None,
                        reasoning_efforts: Some(vec!["low".to_string(), "high".to_string()]),
                        reasoning_default: None,
                        input_modalities: None,
                    },
                    reasoning_effort: None,
                },
                is_active: false,
                engine_name: None,
                selection_target: None,
                selection_preview_error: None,
            }],
            filter: String::new(),
            selected: 0,
        },
        ..TuiState::new()
    };
    let mut textarea = TextArea::default();

    terminal
        .draw(|frame| {
            tui_widgets::draw(
                frame,
                &mut state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            );
        })
        .expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let rendered: String = (0..20u16)
        .flat_map(|row| (0..80u16).map(move |column| (column, row)))
        .map(|(column, row)| buffer.cell((column, row)).unwrap().symbol().to_string())
        .collect();
    assert!(
        !rendered.contains("←/→ effort"),
        "the list no longer adjusts reasoning, so its footer must not say so: {rendered:?}"
    );
    assert!(
        rendered.contains("enter parameters"),
        "rendered footer: {rendered:?}"
    );
    assert!(
        rendered.contains("esc cancel"),
        "rendered footer: {rendered:?}"
    );
    assert!(
        rendered.contains("Engines & Provider Models"),
        "model picker must use the provider models title: {rendered:?}"
    );
    assert!(
        !rendered.contains("adjust reasoning"),
        "model picker title must not duplicate footer controls: {rendered:?}"
    );
    assert!(
        !rendered.contains("enter send"),
        "model picker must not advertise sending: {rendered:?}"
    );
}

#[test]
fn model_picker_details_show_highlighted_compaction_effect() {
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_reducer_state::{ModelChoice, SlashOverlay};
    use ylva::tui_widgets;
    use ylva::{
        CompactionEngineDisplayInfo, EngineDisplayInfo, ModelInfo, ModelSelectionTarget,
        ProviderModelOption, TuiState,
    };

    let make_choice =
        |name: &str, provider_connection: &str, model_id: &str, context_length: usize| {
            ModelChoice {
                catalog_context_length: Some(context_length),
                label: ylva::tui_reducer_state::ChoiceLabel {
                    identity: format!("{name} → {provider_connection}/{model_id}"),
                    details: vec![format!(
                        "context: {}",
                        ylva::llm::token_estimate::format_token_estimate(context_length)
                    )],
                },
                selection: ProviderModelOption {
                    context_length_override: None,
                    provider_connection: provider_connection.to_string(),
                    model: ModelInfo {
                        id: model_id.to_string(),
                        context_length: Some(context_length),
                        reasoning_efforts: None,
                        reasoning_default: None,
                        input_modalities: None,
                    },
                    reasoning_effort: None,
                },
                initial_reasoning_effort: None,
                is_active: name == "engine-a",
                engine_name: Some(name.to_string()),
                selection_target: Some(ModelSelectionTarget {
                    engine_name: Some(name.to_string()),
                    provider_connection: provider_connection.to_string(),
                    model: model_id.to_string(),
                    context_length,
                }),
                selection_preview_error: None,
            }
        };
    let mut state = TuiState {
        engine_display: EngineDisplayInfo {
            engine_name: "engine-a".to_string(),
            provider_connection: "mock".to_string(),
            model: "model-a".to_string(),
            reasoning_effort: None,
            compaction_engine: Some(CompactionEngineDisplayInfo {
                engine_name: "engine-a".to_string(),
                provider_connection: "mock".to_string(),
                model: "model-a".to_string(),
                context_length: 32_768,
                follows_conversation: false,
                shares_conversation: true,
            }),
            inherits_parent_runtime: false,
        },
        slash_overlay: SlashOverlay::ModelPicker {
            parameters: None,
            choices: vec![
                make_choice("engine-a", "mock", "model-a", 131_072),
                make_choice("engine-b", "mock", "model-b", 65_536),
                make_choice("engine-c", "mock", "model-a", 131_072),
                make_choice("engine-d", "mock-alt", "model-a", 131_072),
                make_choice(
                    "openrouter-dense",
                    "openrouter",
                    "openai/gpt-6-luna",
                    98_304,
                ),
            ],
            filter: String::new(),
            selected: 1,
        },
        ..TuiState::new()
    };
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).expect("terminal");
    let mut textarea = TextArea::default();
    let footer_foreground = |terminal: &Terminal<TestBackend>| {
        let buffer = terminal.backend().buffer();
        let row_width = buffer.area.width as usize;
        buffer
            .content()
            .chunks(row_width)
            .find(|row| {
                row.iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>()
                    .contains("Compaction uses")
            })
            .and_then(|row| row.iter().find(|cell| cell.symbol() == "C"))
            .map(|cell| cell.fg)
            .expect("compaction detail foreground")
    };

    terminal
        .draw(|frame| {
            tui_widgets::draw(
                frame,
                &mut state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            );
        })
        .expect("draw");
    let rendered: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(!rendered.contains("Conversation:"));
    assert!(rendered.contains("Compaction uses mock/model-a, context: 32.8K"));
    assert!(!rendered.contains("Compaction uses mock/model-a, context: 32.8K."));
    assert_eq!(footer_foreground(&terminal), Color::Rgb(255, 165, 0));

    if let SlashOverlay::ModelPicker { selected, .. } = &mut state.slash_overlay {
        *selected = 0;
    }
    terminal
        .draw(|frame| {
            tui_widgets::draw(
                frame,
                &mut state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            );
        })
        .expect("redraw");
    let rendered: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(rendered.contains("Compaction uses mock/model-a, context: 131.1K"));
    assert_eq!(footer_foreground(&terminal), Color::DarkGray);

    if let SlashOverlay::ModelPicker { selected, .. } = &mut state.slash_overlay {
        *selected = 2;
    }
    terminal
        .draw(|frame| {
            tui_widgets::draw(
                frame,
                &mut state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            );
        })
        .expect("redraw with matching provider/model");
    let rendered: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(rendered.contains("engine-c → mock/model-a  context: 131.1K"));
    assert!(rendered.contains("Compaction uses mock/model-a, context: 32.8K"));
    assert_eq!(footer_foreground(&terminal), Color::Rgb(255, 165, 0));

    if let SlashOverlay::ModelPicker { selected, .. } = &mut state.slash_overlay {
        *selected = 3;
    }
    terminal
        .draw(|frame| {
            tui_widgets::draw(
                frame,
                &mut state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            );
        })
        .expect("redraw with a different provider connection");
    let rendered: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(rendered.contains("Compaction uses mock/model-a, context: 32.8K"));
    assert_eq!(footer_foreground(&terminal), Color::Rgb(255, 165, 0));

    for (selected, context_length) in [(4, 98_304), (2, 131_072), (3, 131_072)] {
        state
            .engine_display
            .compaction_engine
            .as_mut()
            .expect("compaction engine display")
            .context_length = context_length;
        if let SlashOverlay::ModelPicker {
            selected: highlighted,
            ..
        } = &mut state.slash_overlay
        {
            *highlighted = selected;
        }
        terminal
            .draw(|frame| {
                tui_widgets::draw(
                    frame,
                    &mut state,
                    &mut textarea,
                    None,
                    None,
                    &ylva::ToolRegistry::new(),
                );
            })
            .expect("redraw with matching context windows");
        assert_eq!(
            footer_foreground(&terminal),
            Color::DarkGray,
            "equal context windows must not warn for choice {selected}, even when provider or model differs",
        );
    }

    state
        .engine_display
        .compaction_engine
        .as_mut()
        .expect("compaction engine display")
        .follows_conversation = true;
    if let SlashOverlay::ModelPicker { selected, .. } = &mut state.slash_overlay {
        *selected = 1;
    }
    terminal
        .draw(|frame| {
            tui_widgets::draw(
                frame,
                &mut state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            );
        })
        .expect("redraw with implicit compaction engine");
    let rendered: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(rendered.contains("Compaction uses mock/model-b, context: 65.5K"));
    assert_eq!(footer_foreground(&terminal), Color::DarkGray);
    assert!(!rendered.contains("No summarizer is configured."));

    let mut narrow_state = state;
    let mut narrow_terminal = Terminal::new(TestBackend::new(36, 20)).expect("narrow terminal");
    narrow_terminal
        .draw(|frame| {
            tui_widgets::draw(
                frame,
                &mut narrow_state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            );
        })
        .expect("narrow draw");
    assert!(
        narrow_state.slash_overlay_layout.content_area.height > 0,
        "the picker rows must remain usable when details do not fit"
    );
    let rendered: String = narrow_terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(
        !rendered.contains("Conversation:"),
        "the detail area should yield to picker controls on narrow terminals"
    );
}

#[test]
fn model_picker_click_uses_rendered_row_geometry_without_switching() {
    use ratatui::backend::TestBackend;
    use ratatui_textarea::TextArea;
    use ylva::tui_reducer_state::{AppState, SlashOverlay};
    use ylva::TuiEvent;
    let mut state = AppState::new();
    state.slash_overlay = SlashOverlay::ModelPicker {
        parameters: None,
        choices: model_picker_choices(2),
        filter: String::new(),
        selected: 0,
    };
    let mut terminal = Terminal::new(TestBackend::new(80, 20)).expect("terminal");
    let mut textarea = TextArea::default();

    terminal
        .draw(|frame| {
            ylva::render::render(
                frame,
                &mut state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            );
        })
        .expect("draw");

    let content_area = state.layout.slash_overlay.content_area;
    assert!(
        content_area.height >= 2,
        "both model rows must be represented in the rendered content area"
    );
    assert_eq!(
        state
            .layout
            .slash_overlay
            .row_at(content_area.x, content_area.y + 1),
        Some(1)
    );
    let effects = ylva::presenter::update(
        &mut state,
        TuiEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: content_area.x,
            row: content_area.y + 1,
            modifiers: KeyModifiers::NONE,
        }),
        &empty_textarea_context(),
    );

    assert_eq!(
        state
            .selected_model()
            .map(|choice| choice.selection.model.id.as_str()),
        Some("model-1")
    );
    assert!(
        effects.is_empty(),
        "a rendered-row click must not confirm the model switch: {effects:?}"
    );
}

#[test]
fn model_picker_click_uses_absolute_row_after_scrolling() {
    use ratatui::backend::TestBackend;
    use ratatui_textarea::TextArea;
    use ylva::tui_reducer_state::{AppState, SlashOverlay};
    use ylva::TuiEvent;
    let mut state = AppState::new();
    state.slash_overlay = SlashOverlay::ModelPicker {
        parameters: None,
        choices: model_picker_choices(20),
        filter: String::new(),
        selected: 0,
    };
    let mut terminal = Terminal::new(TestBackend::new(80, 20)).expect("terminal");
    let mut textarea = TextArea::default();

    terminal
        .draw(|frame| {
            ylva::render::render(
                frame,
                &mut state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            );
        })
        .expect("initial draw");
    for _ in 0..10 {
        ylva::presenter::update(
            &mut state,
            TuiEvent::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 1,
                row: 1,
                modifiers: KeyModifiers::NONE,
            }),
            &empty_textarea_context(),
        );
    }
    terminal
        .draw(|frame| {
            ylva::render::render(
                frame,
                &mut state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            );
        })
        .expect("scrolled draw");

    let content_area = state.layout.slash_overlay.content_area;
    let first_visible_row = state.layout.slash_overlay.first_visible_row;
    assert!(
        first_visible_row > 0,
        "the fixture must exercise a scrolled popup"
    );
    let effects = ylva::presenter::update(
        &mut state,
        TuiEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: content_area.x,
            row: content_area.y,
            modifiers: KeyModifiers::NONE,
        }),
        &empty_textarea_context(),
    );

    assert_eq!(
        state
            .selected_model()
            .map(|choice| choice.selection.model.id.as_str()),
        Some(format!("model-{first_visible_row}").as_str())
    );
    assert!(
        effects.is_empty(),
        "a scrolled-row click must not confirm the model switch: {effects:?}"
    );
    let first_visible_row_before_click = first_visible_row;
    terminal
        .draw(|frame| {
            ylva::render::render(
                frame,
                &mut state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            );
        })
        .expect("redraw after click");
    assert_eq!(
        state.layout.slash_overlay.first_visible_row, first_visible_row_before_click,
        "clicking a visible row must not reposition the popup"
    );
}

#[test]
fn model_picker_moves_selection_up_inside_scrolled_window() {
    use ratatui::backend::TestBackend;
    use ratatui_textarea::TextArea;
    use ylva::tui_reducer_state::{AppState, InputEvent, SlashOverlay};
    use ylva::TuiEvent;
    let mut state = AppState::new();
    state.slash_overlay = SlashOverlay::ModelPicker {
        parameters: None,
        choices: model_picker_choices(20),
        filter: String::new(),
        selected: 0,
    };
    let mut terminal = Terminal::new(TestBackend::new(80, 20)).expect("terminal");
    let mut textarea = TextArea::default();
    let textarea_context = empty_textarea_context();

    terminal
        .draw(|frame| {
            ylva::render::render(
                frame,
                &mut state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            );
        })
        .expect("initial draw");
    for _ in 0..10 {
        ylva::presenter::update(
            &mut state,
            TuiEvent::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 1,
                row: 1,
                modifiers: KeyModifiers::NONE,
            }),
            &textarea_context,
        );
        terminal
            .draw(|frame| {
                ylva::render::render(
                    frame,
                    &mut state,
                    &mut textarea,
                    None,
                    None,
                    &ylva::ToolRegistry::new(),
                );
            })
            .expect("scroll draw");
    }
    let first_visible_row = state.layout.slash_overlay.first_visible_row;
    assert!(first_visible_row > 0);
    let current_model_index = state
        .selected_model()
        .and_then(|choice| choice.selection.model.id.strip_prefix("model-"))
        .and_then(|index| index.parse::<usize>().ok())
        .expect("selected model index");
    let moves_inside_window = current_model_index - first_visible_row;
    for step in 1..=moves_inside_window {
        ylva::presenter::update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(KeyEvent::new(
                KeyCode::Up,
                KeyModifiers::NONE,
            ))),
            &textarea_context,
        );
        terminal
            .draw(|frame| {
                ylva::render::render(
                    frame,
                    &mut state,
                    &mut textarea,
                    None,
                    None,
                    &ylva::ToolRegistry::new(),
                );
            })
            .expect("up-navigation draw");
        assert_eq!(
            state
                .selected_model()
                .map(|choice| choice.selection.model.id.as_str()),
            Some(format!("model-{}", current_model_index - step).as_str())
        );
        assert_eq!(
            state.layout.slash_overlay.first_visible_row, first_visible_row,
            "moving within the visible window must not keep the selection at its lower edge"
        );
    }

    ylva::presenter::update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(KeyEvent::new(
            KeyCode::Up,
            KeyModifiers::NONE,
        ))),
        &textarea_context,
    );
    terminal
        .draw(|frame| {
            ylva::render::render(
                frame,
                &mut state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            );
        })
        .expect("upper-bound navigation draw");
    assert_eq!(
        state
            .selected_model()
            .and_then(|choice| choice.selection.model.id.strip_prefix("model-"))
            .and_then(|index| index.parse::<usize>().ok()),
        Some(first_visible_row - 1)
    );
    assert_eq!(
        state.layout.slash_overlay.first_visible_row,
        first_visible_row - 1,
        "the window should move only after selection crosses its upper edge"
    );
}

#[test]
fn model_picker_shows_scrollbar_when_entries_overflow() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_reducer_state::SlashOverlay;
    use ylva::tui_widgets;
    use ylva::TuiState;

    let backend = TestBackend::new(80, 20);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut state = TuiState {
        slash_overlay: SlashOverlay::ModelPicker {
            parameters: None,
            choices: model_picker_choices(20),
            filter: String::new(),
            selected: 0,
        },
        ..TuiState::new()
    };
    let mut textarea = TextArea::default();

    terminal
        .draw(|frame| {
            tui_widgets::draw(
                frame,
                &mut state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            );
        })
        .expect("draw");

    let buffer = terminal.backend().buffer().clone();
    // The popup reserves the last interior column (x=78, between the content
    // and right border) for its scrollbar on this 80-column terminal.
    let scrollbar_symbols: String = (0..20u16)
        .map(|row| buffer.cell((78, row)).unwrap().symbol().to_string())
        .collect();
    assert!(
        scrollbar_symbols.contains('┃'),
        "overflowing model picker must render a scrollbar thumb: {scrollbar_symbols:?}"
    );
    assert!(
        scrollbar_symbols.contains('↓'),
        "overflowing model picker must show a downward scroll indicator: {scrollbar_symbols:?}"
    );
}

/// A live filter that matches nothing must not make the picker vanish
/// entirely — the panel stays open (so the user can keep editing the
/// filter) and shows a message explaining why nothing is listed.
#[test]
fn model_picker_shows_a_message_instead_of_vanishing_when_the_filter_matches_nothing() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_reducer_state::{ModelChoice, SlashOverlay};
    use ylva::tui_widgets;
    use ylva::{ModelInfo, ProviderModelOption, TuiState};

    let backend = TestBackend::new(80, 20);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut state = TuiState {
        slash_overlay: SlashOverlay::ModelPicker {
            parameters: None,
            choices: vec![ModelChoice {
                catalog_context_length: None,
                label: ylva::tui_reducer_state::ChoiceLabel {
                    identity: "stub/thinking".to_string(),
                    details: vec!["reasoning: default".to_string()],
                },
                initial_reasoning_effort: None,
                selection: ProviderModelOption {
                    context_length_override: None,
                    provider_connection: "stub".to_string(),
                    model: ModelInfo {
                        id: "thinking".to_string(),
                        context_length: None,
                        reasoning_efforts: None,
                        reasoning_default: None,
                        input_modalities: None,
                    },
                    reasoning_effort: None,
                },
                is_active: false,
                engine_name: None,
                selection_target: None,
                selection_preview_error: None,
            }],
            filter: "no-such-model".to_string(),
            selected: 0,
        },
        ..TuiState::new()
    };
    let mut textarea = TextArea::default();

    terminal
        .draw(|frame| {
            tui_widgets::draw(
                frame,
                &mut state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            );
        })
        .expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let rendered: String = (0..20u16)
        .flat_map(|row| (0..80u16).map(move |column| (column, row)))
        .map(|(column, row)| buffer.cell((column, row)).unwrap().symbol().to_string())
        .collect();
    assert!(
        rendered.contains("Nothing matched your model filter."),
        "the panel must explain why it is empty instead of vanishing: {rendered:?}"
    );
    assert!(
        rendered.contains("Engines & Provider Models"),
        "the panel must stay open under its usual title: {rendered:?}"
    );
}

#[tokio::test]
async fn click_inside_help_popup_keeps_it_open() {
    let provider = MockProvider::new(vec!["hello".to_string()], Duration::from_millis(200));
    let (backend, last_buffer, _) = InspectableTestBackend::new(80, 24);
    let terminal = Terminal::new(backend).expect("test terminal");
    let (action_tx, action_rx) = mpsc::channel(64);
    let registry = Arc::new(ToolRegistry::new());

    let run_handle = tokio::spawn({
        let mut terminal = terminal;
        async move {
            let engine = Arc::new(EngineImpl::new(
                Arc::new(provider),
                common::test_engine_config(),
            ));
            let (agent, ui_rx) = AgentLoop::new(engine, registry);
            tui_driver::run_tui(
                &mut terminal,
                agent,
                ui_rx,
                None,
                action_rx,
                &mut NoopClipboard,
            )
            .await
            .expect("run_tui completed")
        }
    });

    send(&action_tx, AppAction::ShowHelp).await;
    wait_for_frame(&last_buffer, |buffer| {
        rendered_text(buffer).contains("Help")
    })
    .await;

    // Click well inside the popup (80x24 terminal → popup spans x:10..70, y:4..20).
    send(
        &action_tx,
        AppAction::MouseInput(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 40,
            row: 10,
            modifiers: KeyModifiers::NONE,
        }),
    )
    .await;
    drop(action_tx);

    let state = tokio::time::timeout(Duration::from_secs(5), run_handle)
        .await
        .expect("TUI shutdown completed")
        .expect("task joined");
    assert!(
        state.help_popup.show_help,
        "click inside the help popup must not close it"
    );
}

#[tokio::test]
async fn click_outside_help_popup_closes_it() {
    let provider = MockProvider::new(vec!["hello".to_string()], Duration::from_millis(200));
    let (backend, last_buffer, _) = InspectableTestBackend::new(80, 24);
    let terminal = Terminal::new(backend).expect("test terminal");
    let (action_tx, action_rx) = mpsc::channel(64);
    let registry = Arc::new(ToolRegistry::new());

    let run_handle = tokio::spawn({
        let mut terminal = terminal;
        async move {
            let engine = Arc::new(EngineImpl::new(
                Arc::new(provider),
                common::test_engine_config(),
            ));
            let (agent, ui_rx) = AgentLoop::new(engine, registry);
            tui_driver::run_tui(
                &mut terminal,
                agent,
                ui_rx,
                None,
                action_rx,
                &mut NoopClipboard,
            )
            .await
            .expect("run_tui completed")
        }
    });

    send(&action_tx, AppAction::ShowHelp).await;
    wait_for_frame(&last_buffer, |buffer| {
        rendered_text(buffer).contains("Help")
    })
    .await;

    // Click outside the popup (top-left corner is outside the centered popup).
    send(
        &action_tx,
        AppAction::MouseInput(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        }),
    )
    .await;
    drop(action_tx);

    let state = tokio::time::timeout(Duration::from_secs(5), run_handle)
        .await
        .expect("TUI shutdown completed")
        .expect("task joined");
    assert!(
        !state.help_popup.show_help,
        "click outside the help popup must close it"
    );
}

#[test]
fn help_popup_shows_profile_paths() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use std::path::PathBuf;
    use ylva::{tui_widgets, TuiDisplayMetadata, TuiState};

    let backend = TestBackend::new(120, 80);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut state = TuiState {
        display_metadata: TuiDisplayMetadata {
            profile_name: "work".to_string(),
            profile_path: Some(PathBuf::from("/tmp/ylva-home/profiles/work/config.toml")),
            ylva_home: Some(PathBuf::from("/tmp/ylva-home")),
            workspace_root: Some(PathBuf::from("/workspace/example")),
        },
        ..TuiState::new()
    };
    state.help_popup.show_help = true;
    let mut textarea = TextArea::default();

    terminal
        .draw(|frame| {
            tui_widgets::draw(
                frame,
                &mut state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            );
        })
        .expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let rendered: String = (0..80u16)
        .flat_map(|row| (0..120u16).map(move |column| (column, row)))
        .map(|(column, row)| buffer.cell((column, row)).unwrap().symbol().to_string())
        .collect();
    assert!(
        rendered.contains("  Profile path:   /tmp/ylva-home/profiles/work/config.toml"),
        "help popup should show the resolved profile path: {rendered:?}"
    );
    assert!(
        rendered.contains("Environment"),
        "help popup should label profile and workspace details as environment: {rendered:?}"
    );
    assert!(
        rendered.contains("  Profile:        work"),
        "profile values should align after their labels: {rendered:?}"
    );
    assert!(
        rendered.contains("  Ylva home:      /tmp/ylva-home"),
        "help popup should show the resolved Ylva home: {rendered:?}"
    );
    assert!(
        rendered.contains("  Workspace root: /workspace/example"),
        "help popup should show the discovered workspace root: {rendered:?}"
    );
    let environment_entry_lines = [
        "  Ylva home:      /tmp/ylva-home",
        "  Profile:        work",
        "  Profile path:   /tmp/ylva-home/profiles/work/config.toml",
        "  Workspace root: /workspace/example",
    ];
    let environment_entry_positions: Vec<usize> = environment_entry_lines
        .iter()
        .map(|entry| {
            rendered
                .find(entry)
                .unwrap_or_else(|| panic!("missing environment entry {entry:?}: {rendered:?}"))
        })
        .collect();
    assert!(
        environment_entry_positions
            .windows(2)
            .all(|pair| pair[0] < pair[1]),
        "help popup environment entries should appear in the requested order: {rendered:?}"
    );
    assert!(
        rendered.contains(&format!("  Version:  v{}", ylva::BUILD_INFO.version)),
        "build values should align after their labels: {rendered:?}"
    );
    assert!(
        !rendered.contains("Ylva - Agent-Harness"),
        "help popup should not repeat the build product name: {rendered:?}"
    );
}

/// Two `/models` rows with details long enough to be clipped on narrow terminals.
fn long_model_picker_choices(choice_count: usize) -> Vec<ylva::tui_reducer_state::ModelChoice> {
    use ylva::tui_reducer_state::ModelChoice;
    (0..choice_count)
        .map(|index| ModelChoice {
            catalog_context_length: Some(131_072),
            label: if index == 0 {
                ylva::tui_reducer_state::ChoiceLabel {
                    identity: "shortcut → stub/a-very-long-model-identifier-for-narrow-terminals"
                        .to_string(),
                    details: vec![
                        "context: 131.1K".to_string(),
                        "reasoning: default".to_string(),
                    ],
                }
            } else {
                ylva::tui_reducer_state::ChoiceLabel {
                    identity: format!("stub/other-model-{index}"),
                    details: vec![
                        "context: 65.5K".to_string(),
                        "reasoning: unavailable".to_string(),
                    ],
                }
            },
            initial_reasoning_effort: None,
            selection: ylva::ProviderModelOption {
                context_length_override: None,
                provider_connection: "stub".to_string(),
                model: ylva::ModelInfo {
                    id: format!("model-{index}"),
                    context_length: Some(131_072),
                    reasoning_efforts: None,
                    reasoning_default: None,
                    input_modalities: None,
                },
                reasoning_effort: None,
            },
            is_active: false,
            engine_name: (index == 0).then(|| "shortcut".to_string()),
            selection_target: None,
            selection_preview_error: None,
        })
        .collect()
}

/// Draw the app state into a fresh terminal and return every buffer row as text.
fn draw_app_state_rows(
    state: &mut ylva::AppState,
    width: u16,
    height: u16,
) -> (Vec<String>, Terminal<ratatui::backend::TestBackend>) {
    use ratatui::backend::TestBackend;
    use ratatui_textarea::TextArea;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    let mut textarea = TextArea::default();
    terminal
        .draw(|frame| {
            ylva::render::render(
                frame,
                state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            );
        })
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    let rows = (0..height)
        .map(|row| {
            (0..width)
                .map(|column| buffer.cell((column, row)).unwrap().symbol().to_string())
                .collect()
        })
        .collect();
    (rows, terminal)
}

#[test]
fn model_picker_splits_identity_and_details_into_rows_on_narrow_terminals() {
    use ylva::tui_reducer_state::{AppState, SlashOverlay};
    let mut state = AppState::new();
    state.slash_overlay = SlashOverlay::ModelPicker {
        parameters: None,
        choices: long_model_picker_choices(2),
        filter: String::new(),
        selected: 0,
    };

    let (rows, _terminal) = draw_app_state_rows(&mut state, 50, 30);
    let identity_row = rows
        .iter()
        .position(|row| row.contains("shortcut → "))
        .expect("the identity row must be rendered");
    assert!(
        rows[identity_row].contains('…'),
        "a long identity must be elided to the panel width: {:?}",
        rows[identity_row]
    );
    assert!(
        !rows[identity_row].contains("context:"),
        "details must not share the identity row on narrow terminals: {:?}",
        rows[identity_row]
    );
    assert!(
        rows[identity_row + 1].contains("context: 131.1K  reasoning: default"),
        "the details must render complete on the row below: {:?}",
        rows[identity_row + 1]
    );
    assert!(
        state.layout.slash_overlay.row_layout == ylva::tui_reducer_state::PickerRowLayout::TwoRows,
        "the layout must record the two-row mode for input mapping"
    );
}

#[test]
fn model_picker_keeps_one_row_per_model_on_wide_terminals() {
    use ylva::tui_reducer_state::{AppState, PickerRowLayout, SlashOverlay};
    let mut state = AppState::new();
    state.slash_overlay = SlashOverlay::ModelPicker {
        parameters: None,
        choices: long_model_picker_choices(2),
        filter: String::new(),
        selected: 0,
    };

    let (rows, _terminal) = draw_app_state_rows(&mut state, 120, 30);
    assert!(
        rows.iter().any(|row| row.contains(
            "shortcut → stub/a-very-long-model-identifier-for-narrow-terminals  context: 131.1K"
        )),
        "wide terminals must keep identity and details on one row: {rows:?}"
    );
    assert_eq!(
        state.layout.slash_overlay.row_layout,
        PickerRowLayout::SingleRow
    );
}

#[test]
fn model_picker_click_on_details_row_selects_its_model_on_narrow_terminals() {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ylva::tui_reducer_state::{AppState, SlashOverlay};
    use ylva::TuiEvent;
    let mut state = AppState::new();
    state.slash_overlay = SlashOverlay::ModelPicker {
        parameters: None,
        choices: long_model_picker_choices(2),
        filter: String::new(),
        selected: 0,
    };
    let (_rows, _terminal) = draw_app_state_rows(&mut state, 50, 30);

    // Rows: shortcut identity, shortcut details, catalog identity, catalog details.
    let content_area = state.layout.slash_overlay.content_area;
    let details_row_of_second_model = content_area.y + 3;
    let effects = ylva::presenter::update(
        &mut state,
        TuiEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: content_area.x,
            row: details_row_of_second_model,
            modifiers: KeyModifiers::NONE,
        }),
        &empty_textarea_context(),
    );

    assert_eq!(
        state
            .selected_model()
            .map(|choice| choice.selection.model.id.as_str()),
        Some("model-1"),
        "clicking the details row of a model must highlight that model"
    );
    assert!(
        effects.is_empty(),
        "a click must not confirm the switch: {effects:?}"
    );
}

#[test]
fn model_picker_height_is_capped_at_two_thirds_of_the_space_above_input() {
    use ylva::tui_reducer_state::{AppState, SlashOverlay};
    let mut state = AppState::new();
    state.slash_overlay = SlashOverlay::ModelPicker {
        parameters: None,
        choices: model_picker_choices(60),
        filter: String::new(),
        selected: 0,
    };
    // The popup's inner area (list plus detail rows) is capped; the border
    // rows sit outside that cap. The popup's bottom border touches the input.
    let (rows, _terminal) = draw_app_state_rows(&mut state, 100, 60);
    let space_above_input = state.layout.input_area.rect.y;
    let expected_inner_height = (space_above_input.saturating_sub(1) * 2 / 3) as usize;
    assert!(
        expected_inner_height > 0,
        "the fixture terminal must leave room above the input"
    );
    let title_row = rows
        .iter()
        .position(|row| row.contains("Engines & Provider Models"))
        .expect("the picker title must be rendered");
    let popup_outer_height = space_above_input as usize - title_row;
    assert_eq!(
        popup_outer_height - 2,
        expected_inner_height,
        "a long model list must fill two thirds of the space above the input"
    );
}

#[test]
fn model_picker_marks_the_active_model_by_style_instead_of_a_text_suffix() {
    use ratatui::style::{Color, Modifier};
    use ylva::tui_reducer_state::{AppState, SlashOverlay};
    let mut choices = long_model_picker_choices(3);
    choices[0].is_active = true;
    let mut state = AppState::new();
    state.slash_overlay = SlashOverlay::ModelPicker {
        parameters: None,
        choices,
        filter: String::new(),
        // The third model is highlighted so the second stays an inactive, unselected row.
        selected: 2,
    };

    let (rows, terminal) = draw_app_state_rows(&mut state, 120, 30);
    assert!(
        rows.iter().all(|row| !row.contains("— active")),
        "the active marker must not be rendered as text: {rows:?}"
    );

    let buffer = terminal.backend().buffer();
    let style_of_text = |needle: &str| {
        let row_index = rows
            .iter()
            .position(|row| row.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} must be rendered: {rows:?}"));
        let byte_index = rows[row_index].find(needle).expect("needle is in the row");
        let column = rows[row_index][..byte_index].chars().count();
        let cell = buffer.cell((column as u16, row_index as u16)).unwrap();
        (cell.fg, cell.modifier)
    };

    assert_eq!(
        style_of_text("shortcut → "),
        (Color::Green, Modifier::BOLD),
        "the active model's identity must be green and bold"
    );
    assert_eq!(
        style_of_text("stub/other-model-1"),
        (Color::Gray, Modifier::empty()),
        "inactive models keep the list text style"
    );
}

#[test]
fn model_picker_uses_two_rows_at_any_width_when_a_single_line_would_not_fit() {
    use ylva::tui_reducer_state::{AppState, PickerRowLayout, SlashOverlay};

    // 74 columns is above the footer's narrow threshold, but the long labels
    // do not fit one row, so the picker must still split each model.
    let mut long_state = AppState::new();
    long_state.slash_overlay = SlashOverlay::ModelPicker {
        parameters: None,
        choices: long_model_picker_choices(2),
        filter: String::new(),
        selected: 0,
    };
    let (rows, _terminal) = draw_app_state_rows(&mut long_state, 74, 30);
    assert_eq!(
        long_state.layout.slash_overlay.row_layout,
        PickerRowLayout::TwoRows
    );
    assert!(
        rows.iter()
            .any(|row| row.contains("context: 131.1K  reasoning: default")),
        "the details must be rendered on their own row: {rows:?}"
    );

    // Short labels fit one row at the same width and stay on one row each.
    let mut short_state = AppState::new();
    short_state.slash_overlay = SlashOverlay::ModelPicker {
        parameters: None,
        choices: model_picker_choices(2),
        filter: String::new(),
        selected: 0,
    };
    let (rows, _terminal) = draw_app_state_rows(&mut short_state, 74, 30);
    assert_eq!(
        short_state.layout.slash_overlay.row_layout,
        PickerRowLayout::SingleRow
    );
    assert!(
        rows.iter().any(|row| row.contains("stub/model-0")),
        "short models must keep their single row: {rows:?}"
    );
}

fn press_key(state: &mut ylva::AppState, code: KeyCode) {
    let textarea = empty_textarea_context();
    ylva::presenter::update(
        state,
        ylva::TuiEvent::Input(ylva::tui_reducer_state::InputEvent::KeyInput(
            KeyEvent::new(code, KeyModifiers::NONE),
        )),
        &textarea,
    );
}

#[test]
fn model_picker_page_keys_move_by_the_visible_entry_count() {
    use ylva::tui_reducer_state::{AppState, PickerRowLayout, SlashOverlay};

    // One row per model: a page is the list height in entries.
    let mut state = AppState::new();
    state.slash_overlay = SlashOverlay::ModelPicker {
        parameters: None,
        choices: model_picker_choices(60),
        filter: String::new(),
        selected: 0,
    };
    let (_rows, _terminal) = draw_app_state_rows(&mut state, 100, 30);
    let page = state.layout.slash_overlay.content_area.height as usize;
    assert!(
        page > 3,
        "the fixture must show more than the old fixed step"
    );

    press_key(&mut state, KeyCode::PageDown);
    assert_eq!(
        state
            .selected_model()
            .map(|choice| choice.selection.model.id.as_str()),
        Some(format!("model-{page}").as_str()),
        "PageDown must move by one visible page of single-row entries"
    );
    press_key(&mut state, KeyCode::PageUp);
    assert_eq!(
        state
            .selected_model()
            .map(|choice| choice.selection.model.id.as_str()),
        Some("model-0"),
        "PageUp must return by the same page"
    );

    // Two rows per model: a page holds half as many entries.
    let mut two_row_state = AppState::new();
    two_row_state.slash_overlay = SlashOverlay::ModelPicker {
        parameters: None,
        choices: long_model_picker_choices(60),
        filter: String::new(),
        selected: 0,
    };
    let (_rows, _terminal) = draw_app_state_rows(&mut two_row_state, 50, 30);
    assert_eq!(
        two_row_state.layout.slash_overlay.row_layout,
        PickerRowLayout::TwoRows
    );
    let two_row_page = two_row_state.layout.slash_overlay.content_area.height as usize / 2;
    press_key(&mut two_row_state, KeyCode::PageDown);
    assert_eq!(
        two_row_state
            .selected_model()
            .map(|choice| choice.selection.model.id.as_str()),
        Some(format!("model-{two_row_page}").as_str()),
        "two-row pages must hold the entries that fit, not the rows"
    );
}

#[test]
fn session_picker_page_keys_move_by_the_visible_entry_count() {
    use ylva::tui_reducer_state::{
        AppState, SessionAction, SessionChoice, SessionPicker, SessionPickerPhase, SlashOverlay,
    };

    let mut picker = SessionPicker::loading();
    picker.phase = SessionPickerPhase::Ready;
    picker.action = SessionAction::Resume;
    picker.choices = (0..60)
        .map(|index| SessionChoice {
            session_id: uuid::Uuid::new_v4(),
            title: format!("session {index}"),
            last_used_at: "2026-01-01 12:00:00 +01:00".into(),
        })
        .collect();
    let mut state = AppState::new();
    state.slash_overlay = SlashOverlay::SessionPicker(picker);
    let (_rows, _terminal) = draw_app_state_rows(&mut state, 100, 30);
    let page = state.layout.slash_overlay.content_area.height as usize;
    assert!(
        page > 3,
        "the fixture must show more than the old fixed step"
    );

    press_key(&mut state, KeyCode::PageDown);
    let SlashOverlay::SessionPicker(picker) = &state.slash_overlay else {
        panic!("the session picker must stay open")
    };
    assert_eq!(
        picker.selected, page,
        "PageDown must move by one visible page"
    );
    press_key(&mut state, KeyCode::PageUp);
    let SlashOverlay::SessionPicker(picker) = &state.slash_overlay else {
        panic!("the session picker must stay open")
    };
    assert_eq!(picker.selected, 0, "PageUp must return by the same page");
}

#[test]
fn model_parameter_popup_renders_one_row_per_parameter_with_its_footer_hints() {
    use ylva::tui_reducer_state::{AppState, SlashOverlay};
    let mut choices = long_model_picker_choices(1);
    choices[0].selection.model.reasoning_efforts = Some(vec!["low".to_string()]);
    let mut state = AppState::new();
    state.slash_overlay = SlashOverlay::ModelPicker {
        choices,
        filter: String::new(),
        selected: 0,
        parameters: None,
    };
    state.open_model_parameters();

    let (rows, _terminal) = draw_app_state_rows(&mut state, 120, 30);
    let text: String = rows.join("\n");
    assert!(
        text.contains("Model parameters  ·  shortcut → stub/a-very-long-model-identifier-for-narrow-terminals"),
        "the popup must be titled with the edited model: {text}"
    );
    assert!(text.contains("Context length:  ◂ 131.1K"), "{text}");
    assert!(text.contains("Reasoning effort:    default ▸"), "{text}");

    assert!(
        text.contains("parameter"),
        "popup footer must name the cursor keys: {text}"
    );
    assert!(text.contains("back"), "popup footer must name esc: {text}");
    assert!(
        !text.contains("effort") || !text.contains("←/→ effort"),
        "the list footer must no longer advertise reasoning cycling"
    );
}
