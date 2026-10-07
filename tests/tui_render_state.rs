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

mod common;

use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::backend::{Backend, ClearType, TestBackend, WindowSize};
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::{Position, Size};
use ratatui::Terminal;
use tokio::sync::{mpsc, Notify};
use tokio_util::sync::CancellationToken;

use ylva::tui_driver::NoopClipboard;
use ylva::{
    tui_driver, AgentLoop, AppAction, ChatEntry, Engine, EngineConfig, EngineImpl, Message,
    MockProvider, ModelInfo, StreamEvent, ToolRegistry, ToolSchema, UserMessageState,
};

#[derive(Clone)]
struct InspectableTestBackend {
    backend: TestBackend,
    last_buffer: Arc<std::sync::Mutex<Buffer>>,
    frame_count: Arc<AtomicUsize>,
}

impl InspectableTestBackend {
    fn new(width: u16, height: u16) -> (Self, Arc<std::sync::Mutex<Buffer>>, Arc<AtomicUsize>) {
        let backend = TestBackend::new(width, height);
        let last_buffer = Arc::new(std::sync::Mutex::new(backend.buffer().clone()));
        let frame_count = Arc::new(AtomicUsize::new(0));
        (
            InspectableTestBackend {
                backend,
                last_buffer: Arc::clone(&last_buffer),
                frame_count: Arc::clone(&frame_count),
            },
            last_buffer,
            frame_count,
        )
    }
}

impl Backend for InspectableTestBackend {
    type Error = std::convert::Infallible;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.backend.draw(content)?;
        *self.last_buffer.lock().expect("buffer snapshot lock") = self.backend.buffer().clone();
        self.frame_count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn append_lines(&mut self, line_count: u16) -> Result<(), Self::Error> {
        self.backend.append_lines(line_count)
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.backend.hide_cursor()
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.backend.show_cursor()
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        self.backend.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        self.backend.set_cursor_position(position)
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.backend.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.backend.clear_region(clear_type)
    }

    fn size(&self) -> Result<Size, Self::Error> {
        self.backend.size()
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        self.backend.window_size()
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.backend.flush()
    }
}

fn model_picker_choices(choice_count: usize) -> Vec<ylva::tui_reducer_state::ModelChoice> {
    (0..choice_count)
        .map(|index| ylva::tui_reducer_state::ModelChoice {
            label: format!("stub/model-{index}"),
            initial_reasoning_effort: None,
            selection: ylva::ProviderModelOption {
                provider_connection: "stub".to_string(),
                model: ylva::ModelInfo {
                    id: format!("model-{index}"),
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
        })
        .collect()
}

fn empty_textarea_context() -> ylva::TextareaContext {
    ylva::TextareaContext {
        is_empty: true,
        text: String::new(),
        cursor_row: 0,
        cursor_row_is_last: true,
    }
}

struct DelayedCatalogEngine {
    config: EngineConfig,
    catalog_started: Arc<Notify>,
    release_catalog: Arc<Notify>,
    chat_started: Option<Arc<Notify>>,
    release_chat: Option<Arc<Notify>>,
    hydrate_catalog: bool,
    catalog_hydrated: AtomicBool,
    chat_calls: AtomicUsize,
}

struct CatalogMockProvider {
    provider: MockProvider,
    models: Vec<ModelInfo>,
}

impl ylva::Provider for CatalogMockProvider {
    fn name(&self) -> &str {
        ylva::Provider::name(&self.provider)
    }

    fn list_models(&self) -> futures::future::BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
        Box::pin(async { Ok(self.models.clone()) })
    }

    fn set_model(&self, model: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.models.iter().any(|candidate| candidate.id == model),
            "test model '{model}' is not in the catalog",
        );
        Ok(())
    }

    fn chat<'a>(
        &'a self,
        messages: Vec<Message>,
        tools: Vec<ToolSchema>,
        cancel: CancellationToken,
    ) -> futures::future::BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        ylva::Provider::chat(&self.provider, messages, tools, cancel)
    }
}

impl DelayedCatalogEngine {
    fn new(catalog_started: Arc<Notify>, release_catalog: Arc<Notify>) -> Self {
        DelayedCatalogEngine {
            config: common::test_engine_config(),
            catalog_started,
            release_catalog,
            chat_started: None,
            release_chat: None,
            hydrate_catalog: false,
            catalog_hydrated: AtomicBool::new(false),
            chat_calls: AtomicUsize::new(0),
        }
    }

    fn with_chat(
        catalog_started: Arc<Notify>,
        release_catalog: Arc<Notify>,
        chat_started: Arc<Notify>,
        release_chat: Arc<Notify>,
    ) -> Self {
        DelayedCatalogEngine {
            config: common::test_engine_config(),
            catalog_started,
            release_catalog,
            chat_started: Some(chat_started),
            release_chat: Some(release_chat),
            hydrate_catalog: false,
            catalog_hydrated: AtomicBool::new(false),
            chat_calls: AtomicUsize::new(0),
        }
    }

    fn with_catalog_hydration(mut self) -> Self {
        self.config.context_length = None;
        self.hydrate_catalog = true;
        self
    }
}

impl Engine for DelayedCatalogEngine {
    fn chat<'a>(
        &'a self,
        _messages: Vec<Message>,
        _tools: Vec<ToolSchema>,
        _cancel: CancellationToken,
    ) -> futures::future::BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        let Some(chat_started) = self.chat_started.clone() else {
            return Box::pin(async { anyhow::bail!("the delayed catalog engine does not chat") });
        };
        let release_chat = self
            .release_chat
            .clone()
            .expect("chat release notification configured with chat start");
        Box::pin(async move {
            self.chat_calls.fetch_add(1, Ordering::SeqCst);
            chat_started.notify_one();
            release_chat.notified().await;
            let (stream_tx, stream_rx) = mpsc::channel(4);
            stream_tx
                .send(StreamEvent::Text("conversation update".to_string()))
                .await
                .expect("test stream receiver open");
            stream_tx
                .send(StreamEvent::Done)
                .await
                .expect("test stream receiver open");
            Ok(stream_rx)
        })
    }

    fn config(&self) -> EngineConfig {
        self.config.clone()
    }

    fn list_models(&self) -> futures::future::BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
        let catalog_started = Arc::clone(&self.catalog_started);
        let release_catalog = Arc::clone(&self.release_catalog);
        Box::pin(async move {
            catalog_started.notify_one();
            release_catalog.notified().await;
            Ok(vec![ModelInfo {
                id: "delayed-model".to_string(),
                context_length: None,
                reasoning_efforts: None,
                reasoning_default: None,
                input_modalities: None,
            }])
        })
    }

    fn switch_model(&self, _model: &str) -> anyhow::Result<()> {
        Ok(())
    }

    fn hydrate_catalog_metadata(&self) -> futures::future::BoxFuture<'_, ()> {
        Box::pin(async move {
            if self.hydrate_catalog && !self.catalog_hydrated.load(Ordering::SeqCst) {
                self.list_models().await.expect("catalog lookup succeeds");
                self.catalog_hydrated.store(true, Ordering::SeqCst);
            }
        })
    }
}

/// Type `text` into the (driver-owned) textarea and press Enter.
async fn type_and_enter(tx: &mpsc::Sender<AppAction>, text: &str) {
    for ch in text.chars() {
        tx.send(AppAction::KeyInput(KeyEvent::new(
            KeyCode::Char(ch),
            KeyModifiers::NONE,
        )))
        .await
        .expect("action channel open");
    }
    tx.send(AppAction::KeyInput(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )))
    .await
    .expect("action channel open");
}

async fn send(tx: &mpsc::Sender<AppAction>, action: AppAction) {
    tx.send(action).await.expect("action channel open");
}

fn rendered_text(buffer: &Buffer) -> String {
    (0..buffer.area.height)
        .map(|row| {
            (0..buffer.area.width)
                .map(|column| buffer[(column, row)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
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
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), updates.recv()).await,
        Ok(Some(ylva::UiUpdate::AgentIdle)),
    ));
    for engine_name in ["delayed", "latest"] {
        let provider_connection = format!("{engine_name}-provider");
        let model = format!("{engine_name}-model");
        conversation_engine
            .switch_model_option(&ylva::ProviderModelOption {
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
            assert!(matches!(
                tokio::time::timeout(Duration::from_secs(1), updates.recv()).await,
                Ok(Some(ylva::UiUpdate::AgentIdle)),
            ));
        }
    }
    let update = tokio::time::timeout(Duration::from_secs(1), updates.recv())
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

fn is_assistant_message(entry: &ChatEntry) -> bool {
    matches!(entry, ChatEntry::AssistantMessage { .. })
}

#[tokio::test]
async fn typed_enter_creates_sent_user_bubble_and_assistant() {
    let provider = MockProvider::new(vec!["hello".to_string()], Duration::from_millis(5));
    let backend = TestBackend::new(80, 24);
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

    type_and_enter(&action_tx, "hi").await;
    tokio::time::sleep(Duration::from_millis(80)).await;
    drop(action_tx);

    let state = run_handle.await.expect("task joined");

    assert_eq!(
        state.chat_entries[0],
        ChatEntry::UserMessage {
            text: "hi".to_string(),
            state: UserMessageState::Sent
        }
    );
    assert!(
        state.chat_entries.iter().any(is_assistant_message),
        "assistant bubble should be committed: {:?}",
        state.chat_entries
    );
}

#[tokio::test]
async fn double_press_ctrl_c_mid_stream_cancels_pending_steer() {
    let provider = MockProvider::new(
        vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ],
        Duration::from_millis(40),
    );
    let backend = TestBackend::new(80, 24);
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

    type_and_enter(&action_tx, "first").await;
    tokio::time::sleep(Duration::from_millis(50)).await; // streaming
    type_and_enter(&action_tx, "steer").await; // queued
    send(&action_tx, AppAction::CtrlC).await; // first press
    send(&action_tx, AppAction::CtrlC).await; // second press → Stop
    tokio::time::sleep(Duration::from_millis(120)).await; // let StreamCancelled drain
    drop(action_tx);

    let state = run_handle.await.expect("task joined");

    assert_eq!(state.agent_mode, ylva::AgentMode::Idle);
    let steer = state
        .chat_entries
        .iter()
        .find(|entry| matches!(entry, ChatEntry::UserMessage { text, .. } if text == "steer"))
        .expect("steer bubble present");
    assert_eq!(
        steer,
        &ChatEntry::UserMessage {
            text: "steer".to_string(),
            state: UserMessageState::Cancelled
        }
    );
}

#[tokio::test]
async fn ctrl_c_then_mismatched_escape_does_not_stop_stream() {
    // Regression: Ctrl-C arms its own quit-confirm window. A *different* key
    // (Esc) must not confirm it — it starts Esc's own fresh confirm window
    // instead, leaving the current turn (and any queued steer) to complete
    // normally.
    let provider = MockProvider::new(
        vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ],
        Duration::from_millis(40),
    );
    let backend = TestBackend::new(80, 24);
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

    type_and_enter(&action_tx, "first").await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    type_and_enter(&action_tx, "steer").await;
    send(&action_tx, AppAction::CtrlC).await; // arms ctrl-c's confirm
    send(&action_tx, AppAction::Escape).await; // mismatched key: must not stop
                                               // Wait for both turns to complete naturally.
    tokio::time::sleep(Duration::from_millis(400)).await;
    drop(action_tx);

    let state = run_handle.await.expect("task joined");
    assert_eq!(state.agent_mode, ylva::AgentMode::Idle);
    assert!(
        !state.chat_entries.iter().any(|entry| matches!(
            entry,
            ChatEntry::UserMessage {
                state: UserMessageState::Cancelled,
                ..
            }
        )),
        "queued steer must not be cancelled by a mismatched-key double press: {:?}",
        state.chat_entries
    );
}

#[tokio::test]
async fn steer_cycle_leaves_no_queued_entries() {
    let provider = MockProvider::new(
        vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ],
        Duration::from_millis(30),
    );
    let backend = TestBackend::new(80, 24);
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

    type_and_enter(&action_tx, "first").await;
    tokio::time::sleep(Duration::from_millis(50)).await; // first stream in flight
    type_and_enter(&action_tx, "steer").await; // queued
                                               // Wait for: first stream done → steer injected (SteerApplied) → second
                                               // stream completes → idle.
    tokio::time::sleep(Duration::from_millis(400)).await;
    drop(action_tx);

    let state = run_handle.await.expect("task joined");
    assert!(
        !state.chat_entries.iter().any(|entry| matches!(
            entry,
            ChatEntry::UserMessage {
                state: UserMessageState::Queued,
                ..
            }
        )),
        "no Queued entries should remain after the steer cycle: {:?}",
        state.chat_entries
    );
    assert!(!state.steer_indicator);
}

#[tokio::test]
async fn initial_prompt_produces_sent_bubble() {
    let provider = MockProvider::new(vec!["ok".to_string()], Duration::from_millis(5));
    let backend = TestBackend::new(80, 24);
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
                Some("init".to_string()),
                action_rx,
                &mut NoopClipboard,
            )
            .await
            .expect("run_tui completed")
        }
    });

    tokio::time::sleep(Duration::from_millis(80)).await;
    drop(action_tx);

    let state = run_handle.await.expect("task joined");
    assert_eq!(
        state.chat_entries[0],
        ChatEntry::UserMessage {
            text: "init".to_string(),
            state: UserMessageState::Sent
        }
    );
}

#[tokio::test]
async fn drag_in_chat_area_selects_text() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

    let provider =
        ylva::MockProvider::new(vec!["hello world".to_string()], Duration::from_millis(5));
    let backend = TestBackend::new(80, 24);
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

    // Let the TUI render at least one frame so chat_area is set.
    tokio::time::sleep(Duration::from_millis(30)).await;

    // Simulate a mouse-down over visible wordmark text (row 5).
    send(
        &action_tx,
        AppAction::MouseInput(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 25,
            row: 5,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }),
    )
    .await;

    // Simulate a drag to column 35 on the same row.
    send(
        &action_tx,
        AppAction::MouseInput(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 35,
            row: 5,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }),
    )
    .await;

    // Mouse-up to finalise.
    send(
        &action_tx,
        AppAction::MouseInput(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 35,
            row: 5,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }),
    )
    .await;

    tokio::time::sleep(Duration::from_millis(30)).await;
    drop(action_tx);

    let state = run_handle.await.expect("task joined");
    // The selection should be set and non-empty.
    let sel = state
        .conversation
        .chat_selection
        .expect("chat_selection should be Some after drag");
    assert!(
        !sel.is_empty(),
        "selection should be non-empty after drag: {:?}",
        sel
    );
}

#[test]
fn steer_bubble_renders_below_streaming_not_above() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::AgentMode;

    let backend = TestBackend::new(60, 20);
    let mut terminal = Terminal::new(backend).expect("terminal");

    // Equivalent to: push_user_bubble("hello", Sent) → StreamStarted →
    // AppendText("world") → queue_steer_message("steer me"). Built directly
    // (rather than through a reducer) since this test targets
    // `tui_widgets::draw_chat`'s deferred-rendering order, not state transitions.
    // Queued steers are staged in `queued_steer_texts`, not `chat_entries`
    // (see `AppState::queue_steer_message`).
    let mut state = ylva::TuiState {
        chat_entries: vec![ChatEntry::UserMessage {
            text: "hello".to_string(),
            state: UserMessageState::Sent,
        }],
        queued_steer_texts: vec!["steer me".to_string()],
        partial_assistant_text: "world".to_string(),
        agent_mode: AgentMode::Streaming,
        ..ylva::TuiState::new()
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

    let buf = terminal.backend().buffer().clone();
    // Collect non-empty rows as strings.
    let rows: Vec<String> = (0..20u16)
        .map(|y| {
            (0..60u16)
                .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                .collect::<String>()
        })
        .collect();

    // Find the row that contains "world" (streaming bubble) and "steer me".
    let stream_row = rows
        .iter()
        .position(|r| r.contains("world"))
        .expect("stream text visible");
    let steer_row = rows
        .iter()
        .position(|r| r.contains("steer me"))
        .expect("steer text visible");

    assert!(
        steer_row > stream_row,
        "steer bubble (row {steer_row}) should be BELOW streaming text (row {stream_row})"
    );
}

#[test]
fn steer_sent_before_first_chunk_does_not_jump_above_stream() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::AgentMode;

    let backend = TestBackend::new(60, 20);
    let mut terminal = Terminal::new(backend).expect("terminal");

    // Stream started but no text yet; user already queued a steer.
    let mut state = ylva::TuiState {
        chat_entries: vec![ChatEntry::UserMessage {
            text: "hello".to_string(),
            state: UserMessageState::Sent,
        }],
        queued_steer_texts: vec!["steer me".to_string()],
        agent_mode: AgentMode::Streaming,
        ..ylva::TuiState::new()
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

    // "hello" must appear before "steer me"; no streaming bubble yet (no text).
    let buf = terminal.backend().buffer().clone();
    let rows: Vec<String> = (0..20u16)
        .map(|y| {
            (0..60u16)
                .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                .collect()
        })
        .collect();

    let hello_row = rows
        .iter()
        .position(|r| r.contains("hello"))
        .expect("hello visible");
    let steer_row = rows
        .iter()
        .position(|r| r.contains("steer me"))
        .expect("steer visible");
    assert!(
        steer_row > hello_row,
        "steer (row {steer_row}) should be below hello (row {hello_row})"
    );

    // Now simulate the first chunk arriving; steer must still be below it.
    state.partial_assistant_text.push_str("world");
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
        .expect("draw after chunk");

    let buf = terminal.backend().buffer().clone();
    let rows: Vec<String> = (0..20u16)
        .map(|y| {
            (0..60u16)
                .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                .collect()
        })
        .collect();

    let stream_row = rows
        .iter()
        .position(|r| r.contains("world"))
        .expect("stream text visible");
    let steer_row = rows
        .iter()
        .position(|r| r.contains("steer me"))
        .expect("steer still visible");
    assert!(
        steer_row > stream_row,
        "steer (row {steer_row}) must be below streaming text (row {stream_row})"
    );
}

/// While viewing an *active* subagent's own transcript, the footer must not
/// parrot the root's "Running tool…" word unless the child's own transcript
/// shows one of its own tool bubbles running — otherwise it misleadingly
/// implies the child itself is executing a tool when it's merely
/// streaming/thinking (regression test for the bug where the root's
/// `ToolRunning` mode — reported for the whole lifetime of a live child —
/// bled into the child view's footer verbatim).
#[test]
fn footer_says_subagent_working_while_child_is_only_streaming() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::{AgentMode, ChatEntry, ToolCallState};

    let backend = TestBackend::new(60, 20);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut state = ylva::TuiState {
        agent_mode: AgentMode::ToolRunning,
        child_view: ylva::tui_render_state::ChildViewState {
            child_view_bubble: Some((
                uuid::Uuid::new_v4(),
                "spawn_subagent".to_string(),
                ToolCallState::Running {
                    arguments: serde_json::json!({}),
                    content_sections: vec![],
                    title_arguments: vec![],
                    progress_arguments: vec![],
                },
                false,
            )),
            ..ylva::TuiState::new().child_view
        },
        chat_entries: vec![ChatEntry::AssistantMessage {
            text: "child is replying".to_string(),
            interrupted: false,
        }],
        ..ylva::TuiState::new()
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

    let buf = terminal.backend().buffer().clone();
    let rendered: String = (0..20u16)
        .flat_map(|y| (0..60u16).map(move |x| (x, y)))
        .map(|(x, y)| buf.cell((x, y)).unwrap().symbol().to_string())
        .collect();
    assert!(
        rendered.contains("Subagent working"),
        "footer must say the subagent is working, not that a tool is running: {rendered:?}"
    );
    assert!(
        !rendered.contains("Running tool"),
        "the misleading inherited root mode word must not leak into the child view: {rendered:?}"
    );

    // Once the child's own transcript shows one of *its own* tool bubbles
    // running, "Running tool…" becomes accurate again and must be restored.
    state.chat_entries.push(ChatEntry::ToolBubble {
        id: uuid::Uuid::new_v4(),
        name: "read".to_string(),
        state: Box::new(ToolCallState::Running {
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
            progress_arguments: vec![],
        }),
        collapsed: false,
    });
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
        .expect("draw with child running its own tool");
    let buf = terminal.backend().buffer().clone();
    let rendered: String = (0..20u16)
        .flat_map(|y| (0..60u16).map(move |x| (x, y)))
        .map(|(x, y)| buf.cell((x, y)).unwrap().symbol().to_string())
        .collect();
    assert!(
        rendered.contains("Running tool"),
        "once the child's own transcript shows a running tool bubble, the plain word is accurate again: {rendered:?}"
    );
}

#[test]
fn footer_shows_running_and_queued_subagent_counts() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::{ActiveSubagentCounts, TuiState};

    let backend = TestBackend::new(80, 20);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut state = TuiState {
        active_subagent_counts: ActiveSubagentCounts {
            running: 2,
            queued: 1,
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
        rendered.contains(" · Subagents: 2 running, 1 queued"),
        "subagent counts should follow the busy indicator with a middle-dot separator: {rendered:?}"
    );
}

#[test]
fn footer_shows_the_active_named_engine_and_model() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::{EngineDisplayInfo, TuiState};

    let backend = TestBackend::new(60, 20);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut state = TuiState {
        engine_display: EngineDisplayInfo {
            engine_name: "ollama/local".to_string(),
            provider_connection: "ollama".to_string(),
            model: "llama3.2".to_string(),
            reasoning_effort: Some("max".to_string()),
            compaction_engine: None,
            inherits_parent_runtime: false,
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
        .flat_map(|row| (0..60u16).map(move |column| (column, row)))
        .map(|(column, row)| buffer.cell((column, row)).unwrap().symbol().to_string())
        .collect();
    assert!(
        rendered.contains("ollama/local → llama3.2 (max)"),
        "footer must show the active named-engine/model identity: {rendered:?}"
    );
    assert!(
        !rendered.contains(" → ollama → "),
        "footer must omit the provider connection identity: {rendered:?}"
    );

    state.engine_display.reasoning_effort = None;
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
        .expect("draw without explicit reasoning effort");

    let buffer = terminal.backend().buffer().clone();
    let rendered_without_reasoning: String = (0..20u16)
        .flat_map(|row| (0..60u16).map(move |column| (column, row)))
        .map(|(column, row)| buffer.cell((column, row)).unwrap().symbol().to_string())
        .collect();
    assert!(
        rendered_without_reasoning.contains("ollama/local → llama3.2"),
        "footer must retain the active named-engine/model identity: {rendered_without_reasoning:?}"
    );
    assert!(
        !rendered_without_reasoning.contains("ollama/local → llama3.2 ("),
        "footer must omit the default reasoning effort: {rendered_without_reasoning:?}"
    );
}

#[test]
fn compaction_marker_renders_between_the_summarized_and_the_verbatim_turns() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::{ChatEntry, TuiState, UserMessageState};

    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
    let mut state = TuiState {
        chat_entries: vec![
            ChatEntry::UserMessage {
                text: "the old question".to_string(),
                state: UserMessageState::Sent,
            },
            ChatEntry::AssistantMessage {
                text: "the old answer".to_string(),
                interrupted: false,
            },
            ChatEntry::CompactionMarker {
                summarized_messages: 2,
            },
            ChatEntry::UserMessage {
                text: "the tail question".to_string(),
                state: UserMessageState::Sent,
            },
        ],
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
    let rows: Vec<String> = (0..24u16)
        .map(|row| {
            (0..80u16)
                .map(|column| buffer.cell((column, row)).unwrap().symbol().to_string())
                .collect()
        })
        .collect();
    let rendered = rows.join("\n");
    assert!(
        rendered.contains("compaction · 2 messages summarized"),
        "the marker is rendered: {rendered:?}"
    );
    assert!(
        rendered.contains("the old question") && rendered.contains("the old answer"),
        "the summarized prefix stays browsable: {rendered:?}"
    );
    assert!(
        rendered.contains("the tail question"),
        "the verbatim tail stays browsable: {rendered:?}"
    );
    let marker_row = rows
        .iter()
        .position(|row| row.contains("compaction · 2 messages summarized"))
        .expect("marker row");
    let tail_row = rows
        .iter()
        .position(|row| row.contains("the tail question"))
        .expect("tail row");
    assert!(
        marker_row < tail_row,
        "the marker precedes the tail it opens: marker={marker_row} tail={tail_row}"
    );
}

#[test]
fn footer_shows_compacting_independently_of_agent_mode() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::{tui_widgets, AgentMode, TuiState};

    for agent_mode in [AgentMode::Idle, AgentMode::Streaming] {
        for compacting in [false, true] {
            let mut terminal = Terminal::new(TestBackend::new(100, 20)).expect("terminal");
            let mut state = TuiState {
                agent_mode,
                compacting,
                ..TuiState::new()
            };
            terminal
                .draw(|frame| {
                    tui_widgets::draw(
                        frame,
                        &mut state,
                        &mut TextArea::default(),
                        None,
                        None,
                        &ylva::ToolRegistry::new(),
                    );
                })
                .expect("draw");
            let buffer = terminal.backend().buffer();
            let rendered: String = (0..20u16)
                .flat_map(|row| (0..100u16).map(move |column| (column, row)))
                .map(|position| buffer.cell(position).unwrap().symbol())
                .collect();
            assert_eq!(rendered.contains("Compacting…"), compacting);
            assert_eq!(state.agent_mode, agent_mode);
        }
    }
}

#[test]
fn approval_footer_keeps_context_usage_beside_buttons() {
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::TuiState;

    for is_subagent in [false, true] {
        for denial_reason in ["", "Do not execute this"] {
            let mut terminal = Terminal::new(TestBackend::new(80, 20)).expect("terminal");
            let mut state = TuiState {
                context_usage_text: Some("64.2K (45%)".to_string()),
                pending_approval: Some((
                    uuid::Uuid::new_v4(),
                    "bash".to_string(),
                    serde_json::json!({"command": "echo hello"}),
                )),
                pending_approval_is_subagent: is_subagent,
                ..TuiState::new()
            };
            let mut textarea = TextArea::default();
            textarea.insert_str(denial_reason);
            let mut buttons = None;
            terminal
                .draw(|frame| {
                    buttons = tui_widgets::draw(
                        frame,
                        &mut state,
                        &mut textarea,
                        None,
                        None,
                        &ylva::ToolRegistry::new(),
                    );
                })
                .expect("draw");
            let buttons = buttons.expect("approval buttons");
            let buffer = terminal.backend().buffer();
            let line: String = (0..80)
                .map(|column| buffer.cell((column, buttons.approve.y)).unwrap().symbol())
                .collect();
            assert!(
                line.ends_with("64.2K (45%)"),
                "approval row must retain the right-aligned readout: {line:?}"
            );
            assert!(line.contains("Approve") && line.contains("Deny"));
            assert!(buttons.deny.right() <= 80 - "64.2K (45%)".len() as u16);
        }
    }
}

#[test]
fn footer_shows_the_model_view_readout_right_aligned_on_the_hint_row() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::TuiState;

    let mut terminal = Terminal::new(TestBackend::new(80, 20)).expect("terminal");
    let mut state = TuiState {
        context_usage_text: Some("64.2K (45%)".to_string()),
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
    let hint_row = (0..20u16)
        .find(|row| {
            let line: String = (0..80u16)
                .map(|column| buffer.cell((column, *row)).unwrap().symbol().to_string())
                .collect();
            line.contains("F1 help")
        })
        .expect("keybinding footer row");
    let line: String = (0..80u16)
        .map(|column| {
            buffer
                .cell((column, hint_row))
                .unwrap()
                .symbol()
                .to_string()
        })
        .collect();

    assert!(
        line.contains("64.2K (45%)"),
        "the hint row carries the context readout: {line:?}"
    );
    assert!(
        line.starts_with(' ') && line.contains("enter send"),
        "the keybinding hints stay left-aligned: {line:?}"
    );
    let readout_end = line
        .char_indices()
        .rev()
        .find(|(_, character)| !character.is_whitespace())
        .map(|(index, _)| index)
        .expect("non-blank readout");
    assert_eq!(
        readout_end,
        line.trim_end().len() - 1,
        "the readout sits at the right end of the row: {line:?}"
    );
    assert!(
        !line.contains("enter send   64.2K"),
        "hints and readout must not run together: {line:?}"
    );
}

#[test]
fn narrow_approval_footer_preserves_buttons_without_context_usage() {
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::TuiState;

    let mut terminal = Terminal::new(TestBackend::new(50, 20)).expect("terminal");
    let mut state = TuiState {
        context_usage_text: Some("64.2K (45%)".to_string()),
        pending_approval: Some((
            uuid::Uuid::new_v4(),
            "bash".to_string(),
            serde_json::json!({"command": "echo hello"}),
        )),
        ..TuiState::new()
    };
    let mut textarea = TextArea::default();
    terminal
        .draw(|frame| {
            let buttons = tui_widgets::draw(
                frame,
                &mut state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            )
            .expect("approval buttons");
            assert!(buttons.deny.right() <= 50);
        })
        .expect("draw");
    let buffer = terminal.backend().buffer();
    let rendered: String = (0..20)
        .flat_map(|row| (0..50).map(move |column| buffer.cell((column, row)).unwrap().symbol()))
        .collect();
    assert!(!rendered.contains("64.2K (45%)"), "{rendered:?}");
    assert!(rendered.contains("Approve (enter)") && rendered.contains("Deny (esc)"));
}

#[test]
fn footer_renders_bold_keys_with_normal_action_text_and_separators() {
    use ratatui::backend::TestBackend;
    use ratatui::style::Modifier;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::{AgentMode, TuiState};

    let mut terminal = Terminal::new(TestBackend::new(80, 20)).expect("terminal");
    let mut state = TuiState {
        agent_mode: AgentMode::Streaming,
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
    let footer_row = (0..20u16)
        .find(|row| {
            let line: String = (0..80u16)
                .map(|column| buffer.cell((column, *row)).unwrap().symbol().to_string())
                .collect();
            line.contains("enter send · esc cancel · ctrl-c cancel & quit · F1 help")
        })
        .expect("keybinding footer row");
    let line: String = (0..80u16)
        .map(|column| {
            buffer
                .cell((column, footer_row))
                .unwrap()
                .symbol()
                .to_string()
        })
        .collect();
    let enter_start = line.find("enter").expect("enter keybinding") as u16;
    let send_start = enter_start + "enter ".chars().count() as u16;
    let separator_start = line.find(" · ").expect("separator") as u16;

    for column in enter_start..enter_start + "enter".chars().count() as u16 {
        assert!(
            buffer
                .cell((column, footer_row))
                .expect("enter cell")
                .style()
                .add_modifier
                .contains(Modifier::BOLD),
            "key cell at column {column} should be bold"
        );
    }
    for column in send_start..send_start + "send".chars().count() as u16 {
        assert!(
            !buffer
                .cell((column, footer_row))
                .expect("send cell")
                .style()
                .add_modifier
                .contains(Modifier::BOLD),
            "action cell at column {column} should not be bold"
        );
    }
    for column in separator_start..separator_start + " · ".chars().count() as u16 {
        assert!(
            !buffer
                .cell((column, footer_row))
                .expect("separator cell")
                .style()
                .add_modifier
                .contains(Modifier::BOLD),
            "separator cell at column {column} should not be bold"
        );
    }
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
            choices: vec![ModelChoice {
                label: "stub/thinking  reasoning: default ⚙".to_string(),
                initial_reasoning_effort: None,
                selection: ProviderModelOption {
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
        rendered.contains("←/→ effort"),
        "rendered footer: {rendered:?}"
    );
    assert!(
        rendered.contains("enter switch"),
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

    let make_choice = |name: &str, provider_connection: &str, model_id: &str| {
        let context_length = if model_id == "model-a" {
            131_072
        } else {
            65_536
        };
        ModelChoice {
            label: format!(
                "{name} → {provider_connection}/{model_id}  context: {}kb",
                context_length / 1024
            ),
            selection: ProviderModelOption {
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
            choices: vec![
                make_choice("engine-a", "mock", "model-a"),
                make_choice("engine-b", "mock", "model-b"),
                make_choice("engine-c", "mock", "model-a"),
                make_choice("engine-d", "mock-alt", "model-a"),
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
    assert!(rendered.contains("Compaction uses mock/model-a, context: 32kb"));
    assert!(!rendered.contains("Compaction uses mock/model-a, context: 32kb."));
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
    assert!(rendered.contains("Compaction uses mock/model-a, context: 128kb"));
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
    assert!(rendered.contains("engine-c → mock/model-a  context: 128kb"));
    assert!(rendered.contains("Compaction uses mock/model-a, context: 32kb"));
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
    assert!(rendered.contains("Compaction uses mock/model-a, context: 32kb"));
    assert_eq!(footer_foreground(&terminal), Color::Rgb(255, 165, 0));

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
    assert!(rendered.contains("Compaction uses mock/model-b, context: 64kb"));
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
            choices: vec![ModelChoice {
                label: "stub/thinking  reasoning: default".to_string(),
                initial_reasoning_effort: None,
                selection: ProviderModelOption {
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

/// While viewing a child that is streaming extended-thinking/reasoning text
/// (no visible reply yet), the footer must say "Subagent thinking…" rather
/// than the generic "Subagent working…" — mirrors the root's own
/// "Thinking…"/"Streaming…" distinction, now that the child's own
/// `partial_thinking_text` is projected separately from its
/// `partial_assistant_text` (see `render::project_for_draw`).
#[test]
fn footer_says_subagent_thinking_while_child_streams_thinking_text() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::{AgentMode, ToolCallState};

    let backend = TestBackend::new(60, 20);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut state = ylva::TuiState {
        agent_mode: AgentMode::ToolRunning,
        child_view: ylva::tui_render_state::ChildViewState {
            child_view_bubble: Some((
                uuid::Uuid::new_v4(),
                "spawn_subagent".to_string(),
                ToolCallState::Running {
                    arguments: serde_json::json!({}),
                    content_sections: vec![],
                    title_arguments: vec![],
                    progress_arguments: vec![],
                },
                false,
            )),
            ..ylva::TuiState::new().child_view
        },
        partial_thinking_text: "pondering the flaky test...".to_string(),
        ..ylva::TuiState::new()
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

    let buf = terminal.backend().buffer().clone();
    let rendered: String = (0..20u16)
        .flat_map(|y| (0..60u16).map(move |x| (x, y)))
        .map(|(x, y)| buf.cell((x, y)).unwrap().symbol().to_string())
        .collect();
    assert!(
        rendered.contains("Subagent thinking"),
        "footer must say the subagent is thinking, not generically working: {rendered:?}"
    );
}

/// While viewing a child that is streaming its visible reply text, the
/// footer must say "Subagent streaming…" rather than the generic "Subagent
/// working…" — mirrors the root's own wording for the same case.
#[test]
fn footer_says_subagent_streaming_while_child_streams_response_text() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::{AgentMode, ToolCallState};

    let backend = TestBackend::new(60, 20);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut state = ylva::TuiState {
        agent_mode: AgentMode::ToolRunning,
        child_view: ylva::tui_render_state::ChildViewState {
            child_view_bubble: Some((
                uuid::Uuid::new_v4(),
                "spawn_subagent".to_string(),
                ToolCallState::Running {
                    arguments: serde_json::json!({}),
                    content_sections: vec![],
                    title_arguments: vec![],
                    progress_arguments: vec![],
                },
                false,
            )),
            ..ylva::TuiState::new().child_view
        },
        partial_assistant_text: "here is what I found so far".to_string(),
        ..ylva::TuiState::new()
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

    let buf = terminal.backend().buffer().clone();
    let rendered: String = (0..20u16)
        .flat_map(|y| (0..60u16).map(move |x| (x, y)))
        .map(|(x, y)| buf.cell((x, y)).unwrap().symbol().to_string())
        .collect();
    assert!(
        rendered.contains("Subagent streaming"),
        "footer must say the subagent is streaming, not generically working: {rendered:?}"
    );
}

/// While viewing the *root* transcript, a still-running `spawn_subagent`
/// tool bubble makes `agent_mode()` report `ToolRunning` for the whole
/// lifetime of the live child (see `AppState::agent_mode`) — even while the
/// root loop itself is mid-stream on its own next iteration (e.g. deciding
/// what to do once the child finishes). Before this fix, the in-flight
/// thinking/response bubble was gated on `agent_mode == AgentMode::Streaming`
/// and stayed completely invisible in that window, only appearing once
/// flushed into a fully-committed bubble — i.e. it "popped in" with no
/// visible streaming progress (regression test for
/// docs/known-test-issues.md: "After main agent has spawned multiple subagents
/// ... a thinking bubble pops up in the conversation without any previous
/// thinking-progress").
#[test]
fn root_view_shows_live_thinking_progress_while_a_tool_bubble_is_running() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::{AgentMode, ChatEntry, ToolCallState};

    let backend = TestBackend::new(60, 20);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut state = ylva::TuiState {
        agent_mode: AgentMode::ToolRunning,
        chat_entries: vec![ChatEntry::ToolBubble {
            id: uuid::Uuid::new_v4(),
            name: "spawn_subagent".to_string(),
            state: Box::new(ToolCallState::Running {
                arguments: serde_json::json!({}),
                content_sections: vec![],
                title_arguments: vec![],
                progress_arguments: vec![],
            }),
            collapsed: false,
        }],
        partial_thinking_text: "pondering while the subagent runs...".to_string(),
        ..ylva::TuiState::new()
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

    let buf = terminal.backend().buffer().clone();
    let rendered: String = (0..20u16)
        .flat_map(|y| (0..60u16).map(move |x| (x, y)))
        .map(|(x, y)| buf.cell((x, y)).unwrap().symbol().to_string())
        .collect();
    assert!(
        rendered.contains("pondering while the subagent runs"),
        "in-flight thinking text must render live, not only once flushed: {rendered:?}"
    );

    // Same for in-flight assistant reply text.
    state.partial_thinking_text.clear();
    state.partial_assistant_text = "here is my plan while waiting...".to_string();
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
        .expect("draw with in-flight response text");
    let buf = terminal.backend().buffer().clone();
    let rendered: String = (0..20u16)
        .flat_map(|y| (0..60u16).map(move |x| (x, y)))
        .map(|(x, y)| buf.cell((x, y)).unwrap().symbol().to_string())
        .collect();
    assert!(
        rendered.contains("here is my plan while waiting"),
        "in-flight assistant text must render live, not only once flushed: {rendered:?}"
    );
}

#[test]
fn spawn_subagent_running_duration_is_visible_in_parent_and_child_views() {
    use ratatui_textarea::TextArea;
    use ylva::tui_reducer_state::ResultSection;
    use ylva::{AppState, DomainEvent, TuiEvent};

    for child_view in [false, true] {
        for collapsed in [false, true] {
            let bubble_id = uuid::Uuid::new_v4();
            let run_id = uuid::Uuid::new_v4();
            let mut state = AppState::new();
            let apply_event = |state: &mut AppState, event| {
                ylva::presenter::update(state, TuiEvent::Domain(event), &empty_textarea_context());
            };
            apply_event(
                &mut state,
                DomainEvent::ToolStart {
                    id: bubble_id,
                    name: "spawn_subagent".to_string(),
                    arguments: serde_json::json!({"label": "Inspect"}),
                    content_sections: vec![],
                    title_arguments: vec![("label".to_string(), serde_json::json!("Inspect"))],
                    run_id: None,
                },
            );
            if let ChatEntry::ToolBubble {
                collapsed: bubble_collapsed,
                ..
            } = &mut state.chat_entries[0]
            {
                *bubble_collapsed = collapsed;
            }
            let mut terminal = Terminal::new(TestBackend::new(80, 20)).expect("terminal");
            let mut textarea = TextArea::default();
            let mut draw_title = |state: &mut AppState| {
                terminal
                    .draw(|frame| {
                        ylva::render::render(
                            frame,
                            state,
                            &mut textarea,
                            None,
                            None,
                            &ToolRegistry::new(),
                        );
                    })
                    .expect("draw");
                let lines = if child_view {
                    &state.child_view.header_rendered_lines
                } else {
                    &state.conversation.chat_rendered_lines
                };
                lines
                    .iter()
                    .find(|line| line.contains("Subagent"))
                    .expect("subagent title")
                    .clone()
            };
            if !child_view {
                let title = draw_title(&mut state);
                assert!(
                    !title.contains("0.0s"),
                    "admission is not execution: {title}"
                );
            }
            apply_event(
                &mut state,
                DomainEvent::ChildAgentStarted {
                    run_id,
                    call_id: bubble_id,
                    agent_type: "default".to_string(),
                    task: "Inspect".to_string(),
                    label: "Inspect".to_string(),
                },
            );
            if child_view {
                state.switch_to_child_view(run_id);
            }
            for elapsed_seconds in [12, 22] {
                state
                    .child_conversations
                    .get_mut(&run_id)
                    .expect("child")
                    .started_at =
                    Some(std::time::Instant::now() - Duration::from_secs(elapsed_seconds));
                let started_at = state.child_conversations[&run_id].started_at;
                apply_event(
                    &mut state,
                    DomainEvent::ToolProgress {
                        id: bubble_id,
                        title_arguments: vec![("status".to_string(), serde_json::json!("running"))],
                    },
                );
                assert_eq!(state.child_conversations[&run_id].started_at, started_at);
                let title = draw_title(&mut state);
                let elapsed = title
                    .split_whitespace()
                    .last()
                    .expect("duration")
                    .strip_suffix('s')
                    .expect("seconds suffix")
                    .parse::<f64>()
                    .expect("numeric duration");
                assert!(
                    (elapsed_seconds as f64..elapsed_seconds as f64 + 1.0).contains(&elapsed),
                    "live duration must advance in the {} view: {title}",
                    if child_view { "child" } else { "parent" }
                );
            }
            apply_event(
                &mut state,
                DomainEvent::ToolProgress {
                    id: bubble_id,
                    title_arguments: vec![("status".to_string(), serde_json::json!("queued"))],
                },
            );
            let title = draw_title(&mut state);
            assert!(
                !title.ends_with('s'),
                "queued calls must not show a running duration: {title}"
            );
            apply_event(
                &mut state,
                DomainEvent::ToolDone {
                    id: bubble_id,
                    name: "spawn_subagent".to_string(),
                    result_section: ResultSection::plain("Finished"),
                    summary: vec![],
                    duration: Duration::from_millis(1234),
                    content_sections: vec![],
                    run_id: None,
                },
            );
            let title = draw_title(&mut state);
            assert!(
                title.ends_with("1.234s"),
                "completion must replace the live timer: {title}"
            );
        }
    }
}

/// The drill-in header shortens the root-relative "Spawn Subagent" title.
#[test]
fn child_view_header_shortens_spawn_subagent_title() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::ToolCallState;

    let backend = TestBackend::new(60, 20);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut state = ylva::TuiState {
        child_view: ylva::tui_render_state::ChildViewState {
            child_view_bubble: Some((
                uuid::Uuid::new_v4(),
                "spawn_subagent".to_string(),
                ToolCallState::Running {
                    arguments: serde_json::json!({}),
                    content_sections: vec![],
                    title_arguments: vec![],
                    progress_arguments: vec![],
                },
                false,
            )),
            ..ylva::TuiState::new().child_view
        },
        ..ylva::TuiState::new()
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

    let buf = terminal.backend().buffer().clone();
    let rendered: String = (0..20u16)
        .flat_map(|y| (0..60u16).map(move |x| (x, y)))
        .map(|(x, y)| buf.cell((x, y)).unwrap().symbol().to_string())
        .collect();
    assert!(
        rendered.contains("Subagent"),
        "header must show the shortened title: {rendered:?}"
    );
    assert!(
        !rendered.contains("Spawn Subagent"),
        "the plain title-cased tool name must not appear in the header: {rendered:?}"
    );
}

/// While viewing a finished subagent's transcript, if the root loop is still
/// busy (e.g. producing its final reply from the child's result), the
/// footer must say so explicitly instead of the plain "Streaming…"/"Running
/// tool…" word, which would misleadingly read as if the *subagent* — not
/// the parent — were still working (regression test for the ambiguous
/// footer text bug).
#[test]
fn footer_indicates_subagent_done_while_parent_still_busy() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::{AgentMode, ToolCallState};

    let backend = TestBackend::new(60, 20);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let mut state = ylva::TuiState {
        parent_agent_mode: AgentMode::Streaming,
        agent_mode: AgentMode::Streaming,
        child_view: ylva::tui_render_state::ChildViewState {
            child_view_bubble: Some((
                uuid::Uuid::new_v4(),
                "spawn_subagent".to_string(),
                ToolCallState::Killed,
                false,
            )),
            ..ylva::TuiState::new().child_view
        },
        ..ylva::TuiState::new()
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

    let buf = terminal.backend().buffer().clone();
    let rendered: String = (0..20u16)
        .flat_map(|y| (0..60u16).map(move |x| (x, y)))
        .map(|(x, y)| buf.cell((x, y)).unwrap().symbol().to_string())
        .collect();
    assert!(
        rendered.contains("Subagent done"),
        "footer must call out that the subagent is done while the parent is still busy: {rendered:?}"
    );
    assert!(
        !rendered.contains("Streaming…"),
        "the ambiguous plain mode word must not also be shown: {rendered:?}"
    );

    // Once the root also goes idle, the plain "Idle" word is unambiguous —
    // no special-casing needed (nothing is left "working" to disambiguate).
    state.parent_agent_mode = AgentMode::Idle;
    state.agent_mode = AgentMode::Idle;
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
        .expect("draw after parent idle");
    let buf = terminal.backend().buffer().clone();
    let rendered: String = (0..20u16)
        .flat_map(|y| (0..60u16).map(move |x| (x, y)))
        .map(|(x, y)| buf.cell((x, y)).unwrap().symbol().to_string())
        .collect();
    assert!(
        rendered.contains("Idle"),
        "once the parent is also idle, the plain mode word must show: {rendered:?}"
    );
    assert!(
        !rendered.contains("Subagent done"),
        "the override must not linger once both sides are idle: {rendered:?}"
    );
}

/// Trace the full steer lifecycle visually: stream-1 live → steer queued →
/// stream-1 done → stream-2 live.  Shows what the screen looks like at each
/// phase so we can confirm the expected ordering.
///
/// Each phase's `TuiState` is built directly (the exact result a reducer
/// would have produced) rather than driven through one, since this test
/// targets `tui_widgets::draw_chat`'s rendering order, not state-transition logic
/// (which is covered by `AppState`/`presenter.rs` tests).
#[test]
fn steer_render_order_across_full_steer_cycle() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::AgentMode;

    let render = |state: &mut ylva::TuiState| -> Vec<String> {
        let backend = TestBackend::new(60, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut textarea = TextArea::default();
        terminal
            .draw(|frame| {
                tui_widgets::draw(
                    frame,
                    state,
                    &mut textarea,
                    None,
                    None,
                    &ylva::ToolRegistry::new(),
                );
            })
            .expect("draw");
        let buf = terminal.backend().buffer().clone();
        (0..24u16)
            .map(|y| {
                (0..60u16)
                    .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                    .collect::<String>()
            })
            .collect()
    };

    // Phase 1: stream-1 live, no steer yet — "stream1" should be visible.
    let mut state = ylva::TuiState {
        chat_entries: vec![ChatEntry::UserMessage {
            text: "hello".to_string(),
            state: UserMessageState::Sent,
        }],
        partial_assistant_text: "stream1".to_string(),
        agent_mode: AgentMode::Streaming,
        ..ylva::TuiState::new()
    };
    let rows = render(&mut state);
    assert!(
        rows.iter().any(|r| r.contains("stream1")),
        "stream1 visible"
    );

    // Phase 2: steer queued during stream-1.
    state.queued_steer_texts.push("steer".to_string());
    let rows = render(&mut state);
    let stream_row = rows
        .iter()
        .position(|r| r.contains("stream1"))
        .expect("stream1");
    let steer_row = rows
        .iter()
        .position(|r| r.contains("steer"))
        .expect("steer");
    assert!(
        steer_row > stream_row,
        "phase2: steer (row {steer_row}) must be BELOW stream1 (row {stream_row})"
    );

    // Phase 3: stream-1 done → steer applied → stream-2 starts. This is what
    // `flush_partial_assistant_message` + `commit_queued_steer_messages`
    // would have produced: the committed AssistantMessage sits between the
    // original user message and the (now Steered) steer bubble.
    state.queued_steer_texts.clear();
    state.chat_entries = vec![
        ChatEntry::UserMessage {
            text: "hello".to_string(),
            state: UserMessageState::Sent,
        },
        ChatEntry::AssistantMessage {
            text: "stream1".to_string(),
            interrupted: false,
        },
        ChatEntry::UserMessage {
            text: "steer".to_string(),
            state: UserMessageState::Steered,
        },
    ];
    state.partial_assistant_text = "stream2".to_string();
    let rows = render(&mut state);
    let stream1_row = rows
        .iter()
        .position(|r| r.contains("stream1"))
        .expect("stream1 committed");
    let steer_row = rows
        .iter()
        .position(|r| r.contains("steer"))
        .expect("steer committed");
    let stream2_row = rows
        .iter()
        .position(|r| r.contains("stream2"))
        .expect("stream2");
    assert!(
        stream1_row < steer_row,
        "phase3: stream1 (row {stream1_row}) before steer (row {steer_row})"
    );
    assert!(
        steer_row < stream2_row,
        "phase3: steer (row {steer_row}) before stream2 (row {stream2_row})"
    );
}

/// Message sent while `submitted == true` (provider has message-a but
/// StreamStarted has not yet arrived) must be treated as Queued so the
/// deferral logic keeps it below the eventual streaming bubble.
#[tokio::test]
async fn message_sent_while_submitted_is_queued_not_sent() {
    use std::time::Duration;
    // Slow provider: first chunk arrives after 150 ms, giving us time to
    // type the second message before StreamStarted fires.
    let provider = ylva::MockProvider::new(
        vec!["response-a".to_string(), "response-b".to_string()],
        Duration::from_millis(150),
    );
    let backend = ratatui::backend::TestBackend::new(80, 24);
    let terminal = ratatui::Terminal::new(backend).expect("terminal");
    let (action_tx, action_rx) = tokio::sync::mpsc::channel(64);
    let registry = std::sync::Arc::new(ylva::ToolRegistry::new());

    let run_handle = tokio::spawn({
        let mut terminal = terminal;
        async move {
            let engine = std::sync::Arc::new(ylva::EngineImpl::new(
                std::sync::Arc::new(provider),
                common::test_engine_config(),
            ));
            let (agent, ui_rx) = ylva::AgentLoop::new(engine, registry);
            ylva::tui_driver::run_tui(
                &mut terminal,
                agent,
                ui_rx,
                None,
                action_rx,
                &mut NoopClipboard,
            )
            .await
            .expect("run_tui")
        }
    });

    // Send "message-a"; provider delays before streaming so agent_mode stays
    // Idle / submitted for a moment.
    type_and_enter(&action_tx, "message-a").await;
    // Small pause — enough for the Prompt to reach the agent but not enough
    // for StreamStarted to arrive.
    tokio::time::sleep(Duration::from_millis(20)).await;
    // Send "message-b" while submitted==true, agent_mode==Idle.
    type_and_enter(&action_tx, "message-b").await;
    // Wait for both rounds to complete.
    tokio::time::sleep(Duration::from_millis(800)).await;
    drop(action_tx);

    let state = run_handle.await.expect("join");

    // "message-b" must have been seen as Queued at some point; after the full
    // cycle it transitions to Steered (applied) rather than staying Sent.
    // More importantly the data order must be:
    //   UserMessage(a), AssistantMessage(a), UserMessage(b), AssistantMessage(b)
    let entries = &state.chat_entries;
    assert!(
        entries.len() >= 4,
        "expected ≥4 entries, got {}: {:?}",
        entries.len(),
        entries
    );

    assert!(
        matches!(&entries[0], ChatEntry::UserMessage { text, .. } if text == "message-a"),
        "entry[0] should be user message-a"
    );
    assert!(
        matches!(&entries[1], ChatEntry::AssistantMessage { .. }),
        "entry[1] should be assistant response to a"
    );
    assert!(
        matches!(&entries[2], ChatEntry::UserMessage { text, .. } if text == "message-b"),
        "entry[2] should be user message-b, got {:?}",
        entries[2]
    );
    assert!(
        matches!(&entries[3], ChatEntry::AssistantMessage { .. }),
        "entry[3] should be assistant response to b"
    );
}

#[test]
fn pending_tool_bubble_renders_after_completed_entries() {
    // A ToolBubble in Pending state must be deferred to the end of the
    // rendered conversation so it sits close to the approve/deny footer,
    // even when there are earlier completed entries above it.
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::ToolCallState;

    let backend = TestBackend::new(60, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");

    // An assistant message comes first, then a tool is requested.
    let tool_id = uuid::Uuid::new_v4();
    let mut state = ylva::TuiState {
        chat_entries: vec![
            ChatEntry::UserMessage {
                text: "run something".to_string(),
                state: UserMessageState::Sent,
            },
            ChatEntry::AssistantMessage {
                text: "I will use a tool".to_string(),
                interrupted: false,
            },
            ChatEntry::ToolBubble {
                id: tool_id,
                name: "glob".to_string(),
                state: Box::new(ToolCallState::Pending {
                    arguments: serde_json::json!({"pattern": "**/*.rs"}),
                    content_sections: vec![],
                    title_arguments: vec![],
                }),
                collapsed: true,
            },
        ],
        // The approve/deny footer (drawn only when `pending_approval` is
        // `Some`) is where the lowercase tool name actually renders; the
        // bubble title itself shows the capitalised form ("Glob").
        pending_approval: Some((
            tool_id,
            "glob".to_string(),
            serde_json::json!({"pattern": "**/*.rs"}),
        )),
        ..ylva::TuiState::new()
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

    let buf = terminal.backend().buffer().clone();
    let rows: Vec<String> = (0..30u16)
        .map(|y| {
            (0..60u16)
                .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                .collect::<String>()
        })
        .collect();

    let assistant_row = rows
        .iter()
        .position(|r| r.contains("I will use a tool"))
        .expect("assistant text should be visible");
    let tool_row = rows
        .iter()
        .position(|r| r.contains("glob"))
        .expect("pending tool bubble should be visible");

    assert!(
        tool_row > assistant_row,
        "pending tool bubble (row {tool_row}) should render BELOW assistant text (row {assistant_row})"
    );

    // The tool bubble state in chat_entries must still be Pending.
    let has_pending = state.chat_entries.iter().any(|entry| {
        matches!(
            entry,
            ChatEntry::ToolBubble {
                state,
                ..
            } if matches!(state.as_ref(), ToolCallState::Pending { .. })
        )
    });
    assert!(
        has_pending,
        "chat_entries should still contain a Pending tool bubble"
    );
}

#[test]
fn expanded_tool_bubble_with_multiline_title_argument_stays_single_line_and_gets_a_section() {
    // A multiline scalar title argument (e.g. edit's `old_string`/`new_string`)
    // must not embed raw newlines into the args line of an expanded tool
    // bubble — that breaks the args line's layout by bleeding into
    // neighbouring key=value pairs. Instead it should render as a
    // placeholder-sanitized single line, with the full text available as its
    // own expandable content section.
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::ToolCallState;

    let backend = TestBackend::new(80, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let tool_id = uuid::Uuid::new_v4();
    let mut state = ylva::TuiState {
        chat_entries: vec![ChatEntry::ToolBubble {
            id: tool_id,
            name: "edit".to_string(),
            state: Box::new(ToolCallState::Running {
                arguments: serde_json::json!({
                    "path": "logs/poem.md",
                    "mode": "search_replace",
                    "old_string": "line one\nline two",
                    "new_string": "replaced one\nreplaced two",
                    "replace_all": true,
                }),
                content_sections: vec![],
                title_arguments: vec![
                    ("path".to_string(), serde_json::json!("logs/poem.md")),
                    ("mode".to_string(), serde_json::json!("search_replace")),
                    (
                        "old_string".to_string(),
                        serde_json::json!("line one\nline two"),
                    ),
                    (
                        "new_string".to_string(),
                        serde_json::json!("replaced one\nreplaced two"),
                    ),
                    ("replace_all".to_string(), serde_json::json!(true)),
                ],
                progress_arguments: vec![],
            }),
            collapsed: false,
        }],
        ..ylva::TuiState::new()
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

    let buf = terminal.backend().buffer().clone();
    let rows: Vec<String> = (0..30u16)
        .map(|y| {
            (0..80u16)
                .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                .collect::<String>()
        })
        .collect();

    // The args line renders "old_string=<value>" as a single sanitized row
    // (word-wrapping to the next row afterwards is fine — what must not
    // happen is a raw embedded newline splitting the *value* itself).
    let args_row = rows
        .iter()
        .position(|r| r.contains("old_string="))
        .expect("args row containing old_string= should be present");
    assert!(
        rows[args_row].contains('↵'),
        "embedded newline should render as a placeholder glyph, got: {:?}",
        rows[args_row]
    );

    // The full multiline value must still be readable, as its own labelled
    // content section in the expanded body.
    assert!(
        rows.iter().any(|r| r.contains("old_string:")),
        "expanded body should contain a labelled old_string content section"
    );
    assert!(
        rows.iter().any(|r| r.contains("line one")) && rows.iter().any(|r| r.contains("line two")),
        "full old_string text should be visible across its own content-section lines"
    );
}

/// Render one tool bubble and return the visible rows as strings.
fn render_tool_bubble_rows(state: &mut ylva::TuiState, width: u16, height: u16) -> Vec<String> {
    render_tool_bubble_rows_with_registry(state, width, height, &ylva::ToolRegistry::new())
}

/// Render one tool bubble against an explicit `registry` — a tool's display
/// title and subtype qualifier resolve through it, exactly as in production.
fn render_tool_bubble_rows_with_registry(
    state: &mut ylva::TuiState,
    width: u16,
    height: u16,
    registry: &ylva::ToolRegistry,
) -> Vec<String> {
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;

    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut textarea = TextArea::default();
    terminal
        .draw(|frame| {
            tui_widgets::draw(frame, state, &mut textarea, None, None, registry);
        })
        .expect("draw");
    let buf = terminal.backend().buffer().clone();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                .collect::<String>()
        })
        .collect()
}

fn bash_label_command_sections() -> Vec<ylva::agent::tools::Section> {
    use ylva::agent::tools::{ContentSection, Section};
    vec![
        Section::Content(ContentSection {
            label: Some("Label".to_string()),
            content: "Greet the user".to_string(),
            file_extension: None,
            line_number_offset: None,
        }),
        Section::Content(ContentSection {
            label: Some("Command".to_string()),
            content: "echo hi".to_string(),
            file_extension: None,
            line_number_offset: None,
        }),
    ]
}

#[test]
fn expanded_bash_bubble_hides_covered_args_and_orders_label_before_command() {
    use ylva::ToolCallState;

    let tool_id = uuid::Uuid::new_v4();
    let mut state = ylva::TuiState {
        chat_entries: vec![ChatEntry::ToolBubble {
            id: tool_id,
            name: "bash".to_string(),
            state: Box::new(ToolCallState::Pending {
                arguments: serde_json::json!({
                    "command": "echo hi",
                    "label": "Greet the user",
                }),
                content_sections: bash_label_command_sections(),
                title_arguments: vec![
                    ("command".to_string(), serde_json::json!("echo hi")),
                    ("label".to_string(), serde_json::json!("Greet the user")),
                ],
            }),
            collapsed: false,
        }],
        ..ylva::TuiState::new()
    };
    let rows = render_tool_bubble_rows(&mut state, 80, 30);

    let label_row = rows
        .iter()
        .position(|row| row.contains("Label:"))
        .expect("Label header should render");
    let command_row = rows
        .iter()
        .position(|row| row.contains("Command:"))
        .expect("Command header should render");
    assert!(
        label_row < command_row,
        "Label section must come before Command: label_row={label_row}, command_row={command_row}"
    );
    assert!(
        !rows.iter().any(|row| row.contains("label=")),
        "covered args line must be hidden when expanded, rows: {rows:?}"
    );
    assert_eq!(
        rows.iter().filter(|row| row.contains("echo hi")).count(),
        1,
        "command must appear exactly once (section body, not args line): {rows:?}"
    );
}

#[test]
fn collapsed_bash_bubble_keeps_command_and_label_in_title() {
    use ylva::ToolCallState;

    let tool_id = uuid::Uuid::new_v4();
    let mut state = ylva::TuiState {
        chat_entries: vec![ChatEntry::ToolBubble {
            id: tool_id,
            name: "bash".to_string(),
            state: Box::new(ToolCallState::Pending {
                arguments: serde_json::json!({
                    "command": "echo hi",
                    "label": "Greet the user",
                }),
                content_sections: bash_label_command_sections(),
                title_arguments: vec![
                    ("command".to_string(), serde_json::json!("echo hi")),
                    ("label".to_string(), serde_json::json!("Greet the user")),
                ],
            }),
            collapsed: true,
        }],
        ..ylva::TuiState::new()
    };
    let rows = render_tool_bubble_rows(&mut state, 80, 30);

    let title_row = rows
        .iter()
        .find(|row| row.contains("Bash"))
        .expect("collapsed title row should render");
    assert!(
        title_row.contains("echo hi"),
        "collapsed title must keep the command primary: {title_row}"
    );
    assert!(
        title_row.contains("label=Greet the user"),
        "collapsed title must keep the label prop: {title_row}"
    );
}

#[test]
fn expanded_bash_multiline_command_renders_one_command_section() {
    use ylva::agent::tools::{ContentSection, Section};
    use ylva::ToolCallState;

    let multiline_command = "echo one\necho two";
    let tool_id = uuid::Uuid::new_v4();
    let mut state = ylva::TuiState {
        chat_entries: vec![ChatEntry::ToolBubble {
            id: tool_id,
            name: "bash".to_string(),
            state: Box::new(ToolCallState::Pending {
                arguments: serde_json::json!({
                    "command": multiline_command,
                    "label": "Two echoes",
                }),
                content_sections: vec![
                    Section::Content(ContentSection {
                        label: Some("Label".to_string()),
                        content: "Two echoes".to_string(),
                        file_extension: None,
                        line_number_offset: None,
                    }),
                    Section::Content(ContentSection {
                        label: Some("Command".to_string()),
                        content: multiline_command.to_string(),
                        file_extension: None,
                        line_number_offset: None,
                    }),
                ],
                title_arguments: vec![
                    ("command".to_string(), serde_json::json!(multiline_command)),
                    ("label".to_string(), serde_json::json!("Two echoes")),
                ],
            }),
            collapsed: false,
        }],
        ..ylva::TuiState::new()
    };
    let rows = render_tool_bubble_rows(&mut state, 80, 30);

    let label_row = rows
        .iter()
        .position(|row| row.contains("Label:"))
        .expect("Label header should render");
    let command_row = rows
        .iter()
        .position(|row| row.contains("Command:"))
        .expect("Command header should render");
    assert!(label_row < command_row, "Label must precede Command");
    assert!(
        !rows.iter().any(|row| row.contains("label=")),
        "covered args line must be hidden for multiline commands too: {rows:?}"
    );
    assert_eq!(
        rows.iter().filter(|row| row.contains("echo one")).count(),
        1,
        "multiline command must render exactly once (dedup guard), rows: {rows:?}"
    );
    assert_eq!(
        rows.iter().filter(|row| row.contains("echo two")).count(),
        1,
        "multiline command must render exactly once (dedup guard), rows: {rows:?}"
    );
}

#[test]
fn expanded_bash_command_section_renders_with_syntax_colors() {
    // The Command section carries `file_extension: "sh"`, so the renderer
    // picks the shell grammar: the command row must contain more than one
    // foreground color (comment token vs default), which the plain-text
    // path (single theme-default fg) never produces.
    use ratatui_textarea::TextArea;
    use ylva::tui_widgets;
    use ylva::ToolCallState;

    let command = "echo hi # trailing comment";
    let registry = ylva::default_registry(
        std::sync::Arc::new(std::sync::RwLock::new(ylva::Sandbox::new())),
        ylva::ToolRegistryConfig::default(),
    );
    let call = registry
        .create_call(
            "bash",
            uuid::Uuid::new_v4(),
            serde_json::json!({
                "command": command,
                "label": "Greet the user",
            }),
        )
        .expect("bash tool call");
    let content_sections = call.pending_content_sections();

    let tool_id = uuid::Uuid::new_v4();
    let mut state = ylva::TuiState {
        chat_entries: vec![ChatEntry::ToolBubble {
            id: tool_id,
            name: "bash".to_string(),
            state: Box::new(ToolCallState::Pending {
                arguments: serde_json::json!({
                    "command": command,
                    "label": "Greet the user",
                }),
                content_sections,
                title_arguments: vec![
                    ("command".to_string(), serde_json::json!(command)),
                    ("label".to_string(), serde_json::json!("Greet the user")),
                ],
            }),
            collapsed: false,
        }],
        ..ylva::TuiState::new()
    };

    let backend = TestBackend::new(80, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
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

    let rows: Vec<String> = (0..30u16)
        .map(|y| {
            (0..80u16)
                .map(|x| buffer.cell((x, y)).unwrap().symbol().to_string())
                .collect()
        })
        .collect();
    let command_row = rows
        .iter()
        .position(|row| row.contains("trailing comment"))
        .expect("command body row should render");

    // Skip the two-cell bar+margin prefix; ignore spaces between tokens.
    let distinct_foregrounds: std::collections::HashSet<_> = (2..80u16)
        .map(|x| buffer.cell((x, command_row as u16)).unwrap())
        .filter(|cell| cell.symbol() != " ")
        .filter_map(|cell| cell.style().fg)
        .collect();
    assert!(
        distinct_foregrounds.len() >= 2,
        "command row should be syntax-highlighted (>=2 foreground colors), got {distinct_foregrounds:?} — rows: {rows:?}"
    );
}

#[test]
fn expanded_bash_done_bubble_renders_result_header_last() {
    use ylva::tui_reducer_state::ResultSection;
    use ylva::ToolCallState;

    let tool_id = uuid::Uuid::new_v4();
    let mut state = ylva::TuiState {
        chat_entries: vec![ChatEntry::ToolBubble {
            id: tool_id,
            name: "bash".to_string(),
            state: Box::new(ToolCallState::Done {
                arguments: serde_json::json!({
                    "command": "echo hi",
                    "label": "Greet the user",
                }),
                result_section: ResultSection::plain("hi\n"),
                content_sections: vec![
                    ylva::agent::tools::Section::Content(ylva::agent::tools::ContentSection {
                        label: Some("Label".to_string()),
                        content: "Greet the user".to_string(),
                        file_extension: None,
                        line_number_offset: None,
                    }),
                    ylva::agent::tools::Section::Content(ylva::agent::tools::ContentSection {
                        label: Some("Command".to_string()),
                        content: "echo hi".to_string(),
                        file_extension: None,
                        line_number_offset: None,
                    }),
                    ylva::agent::tools::Section::Content(ylva::agent::tools::ContentSection {
                        label: Some("Result".to_string()),
                        content: "hi\n".to_string(),
                        file_extension: None,
                        line_number_offset: Some(0),
                    }),
                ],
                summary: vec![],
                duration: std::time::Duration::from_millis(12),
                title_arguments: vec![
                    ("command".to_string(), serde_json::json!("echo hi")),
                    ("label".to_string(), serde_json::json!("Greet the user")),
                ],
            }),
            collapsed: false,
        }],
        ..ylva::TuiState::new()
    };
    let rows = render_tool_bubble_rows(&mut state, 80, 30);

    let label_row = rows
        .iter()
        .position(|row| row.contains("Label:"))
        .expect("Label header should render");
    let command_row = rows
        .iter()
        .position(|row| row.contains("Command:"))
        .expect("Command header should render");
    let result_row = rows
        .iter()
        .position(|row| row.contains("Result:"))
        .expect("Result header should render");
    assert!(
        label_row < command_row && command_row < result_row,
        "headers must render Label, Command, Result in order: {label_row} {command_row} {result_row}"
    );
    assert!(
        !rows.iter().any(|row| row.contains("label=")),
        "covered args line must stay hidden in Done: {rows:?}"
    );
    assert!(
        rows[result_row..].iter().any(|row| row.contains("hi")),
        "result content should render under the Result header: {rows:?}"
    );
}

#[test]
fn expanded_bubble_shows_args_when_sections_do_not_cover_title_keys() {
    // Data-driven gate: a section whose label doesn't match the title keys
    // (here `write`'s unlabeled content preview) must NOT hide the args line.
    use ylva::agent::tools::{ContentSection, Section};
    use ylva::ToolCallState;

    let tool_id = uuid::Uuid::new_v4();
    let mut state = ylva::TuiState {
        chat_entries: vec![ChatEntry::ToolBubble {
            id: tool_id,
            name: "write".to_string(),
            state: Box::new(ToolCallState::Pending {
                arguments: serde_json::json!({
                    "path": "src/main.rs",
                    "content": "fn main() {}",
                }),
                content_sections: vec![Section::Content(ContentSection {
                    label: None,
                    content: "fn main() {}".to_string(),
                    file_extension: Some("rs".to_string()),
                    line_number_offset: Some(0),
                })],
                title_arguments: vec![("path".to_string(), serde_json::json!("src/main.rs"))],
            }),
            collapsed: false,
        }],
        ..ylva::TuiState::new()
    };
    let rows = render_tool_bubble_rows(&mut state, 80, 30);

    assert!(
        rows.iter().any(|row| row.contains("src/main.rs")),
        "args line must still show the uncovered title key, rows: {rows:?}"
    );
}

#[test]
fn collapsed_bubble_caps_title_args_with_the_ellipsis_glyph() {
    use ylva::tui_reducer_state::ResultSection;
    use ylva::ToolCallState;

    // Long enough that the collapsed title must cut it at any sane width.
    let long_terms = "agent-harnesses that can be run on MacOS with a very long query ".repeat(4);
    let mut state = ylva::TuiState {
        chat_entries: vec![ChatEntry::ToolBubble {
            id: uuid::Uuid::new_v4(),
            name: "web_search".to_string(),
            state: Box::new(ToolCallState::Done {
                arguments: serde_json::json!({ "terms": long_terms }),
                result_section: ResultSection::plain(""),
                content_sections: vec![],
                summary: vec![],
                duration: std::time::Duration::from_millis(1234),
                title_arguments: vec![("terms".to_string(), serde_json::json!(long_terms))],
            }),
            collapsed: true,
        }],
        ..ylva::TuiState::new()
    };

    let rows = render_tool_bubble_rows(&mut state, 60, 20);
    let title_row = rows
        .iter()
        .find(|row| row.contains("Web Search"))
        .expect("the collapsed title row renders");

    assert!(
        title_row.contains('…'),
        "capped title args must end in the ellipsis glyph: {title_row:?}"
    );
    assert!(
        !title_row.contains("..."),
        "three plain dots must not be used as the cap marker: {title_row:?}"
    );
    assert!(
        !title_row.contains(" …"),
        "the cut must not leave a space before the marker: {title_row:?}"
    );
    assert!(
        title_row.contains('→'),
        "the duration summary stays visible after the marker: {title_row:?}"
    );
    assert!(
        title_row.contains("… → 1.234s"),
        "the marker is followed by the duration summary: {title_row:?}"
    );
}

#[test]
fn collapsed_bubble_title_cut_drops_a_space_before_the_marker() {
    use ylva::tui_reducer_state::ResultSection;
    use ylva::ToolCallState;

    // Word followed by enough spaces that any realistic cut lands inside the
    // run — without trimming, the marker would render as `word     …`.
    let long_terms = format!("agent-harnesses{}", " ".repeat(400));
    let mut state = ylva::TuiState {
        chat_entries: vec![ChatEntry::ToolBubble {
            id: uuid::Uuid::new_v4(),
            name: "web_search".to_string(),
            state: Box::new(ToolCallState::Done {
                arguments: serde_json::json!({ "terms": long_terms }),
                result_section: ResultSection::plain(""),
                content_sections: vec![],
                summary: vec![],
                duration: std::time::Duration::from_millis(1234),
                title_arguments: vec![("terms".to_string(), serde_json::json!(long_terms))],
            }),
            collapsed: true,
        }],
        ..ylva::TuiState::new()
    };

    let rows = render_tool_bubble_rows(&mut state, 120, 20);
    let title_row = rows
        .iter()
        .find(|row| row.contains("agent-harnesses"))
        .expect("the collapsed title row renders");

    assert!(
        title_row.contains("agent-harnesses…"),
        "the marker must hug the last visible character: {title_row:?}"
    );
    assert!(
        !title_row.contains(" …"),
        "the cut must not leave whitespace before the marker: {title_row:?}"
    );
}

#[test]
fn subagent_bubble_title_carries_the_agent_type_as_a_subtype() {
    use ylva::agent::tools::spawn_subagent::SpawnSubagentTool;
    use ylva::tui_reducer_state::ResultSection;
    use ylva::ToolCallState;

    let mut registry = ylva::ToolRegistry::new();
    registry.register(Arc::new(SpawnSubagentTool::new(false)));

    let mut state = ylva::TuiState {
        chat_entries: vec![ChatEntry::ToolBubble {
            id: uuid::Uuid::new_v4(),
            name: SpawnSubagentTool::NAME.to_string(),
            state: Box::new(ToolCallState::Done {
                arguments: serde_json::json!({
                    "label": "Analyze read tool features",
                    "task": "list the read tool's features",
                    "agent_type": "local-discovery",
                }),
                result_section: ResultSection::plain(""),
                content_sections: vec![],
                summary: vec![("status".to_string(), "unstructured".to_string())],
                duration: std::time::Duration::from_millis(48_392),
                title_arguments: vec![
                    (
                        "label".to_string(),
                        serde_json::json!("Analyze read tool features"),
                    ),
                    (
                        "agent_type".to_string(),
                        serde_json::json!("local-discovery"),
                    ),
                ],
            }),
            collapsed: true,
        }],
        ..ylva::TuiState::new()
    };

    let rows = render_tool_bubble_rows_with_registry(&mut state, 120, 20, &registry);
    let title_row = rows
        .iter()
        .find(|row| row.contains("Spawn Subagent"))
        .expect("the collapsed subagent title row renders");

    assert!(
        title_row.contains("Spawn Subagent (local-discovery)"),
        "the subtype reads as part of the title: {title_row:?}"
    );
    assert!(
        !title_row.contains("agent_type"),
        "the subtype must not be rendered a second time as a prop: {title_row:?}"
    );
    assert!(
        title_row.contains("Analyze read tool features"),
        "the label stays the inline primary: {title_row:?}"
    );
    assert!(
        title_row.contains("status=unstructured"),
        "the summary is untouched by the subtype promotion: {title_row:?}"
    );
}

#[tokio::test]
async fn escape_twice_while_streaming_stops_stream_but_keeps_tui_open() {
    // Esc+Esc must stop the agent but NOT exit the TUI (unlike Ctrl+C+Ctrl+C).
    let provider = MockProvider::new(
        vec!["a".to_string(), "b".to_string(), "c".to_string()],
        Duration::from_millis(40),
    );
    let backend = TestBackend::new(80, 24);
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

    type_and_enter(&action_tx, "hello").await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    send(&action_tx, AppAction::Escape).await; // first press: enter confirm
    send(&action_tx, AppAction::Escape).await; // second press: send Stop, stay in TUI
    tokio::time::sleep(Duration::from_millis(120)).await;

    // TUI must still be running after Esc+Esc.
    assert!(
        !run_handle.is_finished(),
        "TUI must remain open after Esc+Esc"
    );
    drop(action_tx);

    let state = run_handle.await.expect("task joined");
    assert_eq!(state.agent_mode, ylva::AgentMode::Idle);
}

#[tokio::test]
async fn click_inside_help_popup_keeps_it_open() {
    let provider = MockProvider::new(vec!["hello".to_string()], Duration::from_millis(200));
    let backend = TestBackend::new(80, 24);
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
    tokio::time::sleep(Duration::from_millis(50)).await; // let the popup render, setting its rect

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
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(action_tx);

    let state = run_handle.await.expect("task joined");
    assert!(
        state.help_popup.show_help,
        "click inside the help popup must not close it"
    );
}

#[tokio::test]
async fn click_outside_help_popup_closes_it() {
    let provider = MockProvider::new(vec!["hello".to_string()], Duration::from_millis(200));
    let backend = TestBackend::new(80, 24);
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
    tokio::time::sleep(Duration::from_millis(50)).await;

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
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(action_tx);

    let state = run_handle.await.expect("task joined");
    assert!(
        !state.help_popup.show_help,
        "click outside the help popup must close it"
    );
}

#[test]
fn dragging_across_logo_build_lines_copies_version_and_profile() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::{
        presenter, render, AppState, Effect, TextareaContext, TuiDisplayMetadata, TuiEvent,
        BUILD_INFO,
    };

    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut state = AppState::new();
    state.display_metadata = TuiDisplayMetadata {
        profile_name: "work".to_string(),
        ..TuiDisplayMetadata::default()
    };
    let mut textarea = TextArea::default();

    terminal
        .draw(|frame| {
            render::render(
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
    let rows: Vec<String> = (0..30u16)
        .map(|row| {
            (0..100u16)
                .map(|column| buffer.cell((column, row)).unwrap().symbol().to_string())
                .collect()
        })
        .collect();
    let version_line = format!("v{}  {}", BUILD_INFO.version, BUILD_INFO.git_commit);
    let version_row = rows
        .iter()
        .position(|row| row.contains(&version_line))
        .expect("version line should be rendered");
    let profile_row = rows
        .iter()
        .position(|row| row.contains("profile: work"))
        .expect("profile line should be rendered");
    let version_column = rows[version_row]
        .find(&version_line)
        .expect("version line start should be visible") as u16;
    let profile_end_column = (rows[profile_row]
        .find("profile: work")
        .expect("profile line start should be visible")
        + "profile: work".len()) as u16;

    let textarea_context = TextareaContext {
        is_empty: true,
        text: String::new(),
        cursor_row: 0,
        cursor_row_is_last: true,
    };
    presenter::update(
        &mut state,
        TuiEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: version_column,
            row: version_row as u16,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }),
        &textarea_context,
    );
    presenter::update(
        &mut state,
        TuiEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: profile_end_column,
            row: profile_row as u16,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }),
        &textarea_context,
    );
    let effects = presenter::update(
        &mut state,
        TuiEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: profile_end_column,
            row: profile_row as u16,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }),
        &textarea_context,
    );

    let expected = format!(
        "v{}  {}\nprofile: work",
        BUILD_INFO.version, BUILD_INFO.git_commit
    );
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::CopyToClipboard(text) if text == &expected)),
        "dragging the logo should copy both build lines without alignment padding: {effects:?}"
    );
}

#[test]
fn startup_logo_shows_selected_profile() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::{tui_widgets, TuiDisplayMetadata, TuiState};

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut state = TuiState {
        display_metadata: TuiDisplayMetadata {
            profile_name: "work".to_string(),
            ..TuiDisplayMetadata::default()
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
    let rendered: String = (0..24u16)
        .flat_map(|row| (0..80u16).map(move |column| (column, row)))
        .map(|(column, row)| buffer.cell((column, row)).unwrap().symbol().to_string())
        .collect();
    assert!(
        rendered.contains("profile: work"),
        "startup logo should show the selected profile: {rendered:?}"
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

#[test]
fn dragging_scrolled_help_popup_line_copies_rendered_text() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_textarea::TextArea;
    use ylva::{presenter, render, AppState, Effect, Focus, TextareaContext, TuiEvent};

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let mut state = AppState::new();
    state.focus = Focus::HelpPopup;
    let mut textarea = TextArea::default();

    terminal
        .draw(|frame| {
            render::render(
                frame,
                &mut state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            );
        })
        .expect("initial help popup draw");

    let target_line = state
        .help_popup
        .help_rendered_lines
        .iter()
        .position(|line| line == "Help popup")
        .expect("help popup heading should be rendered");
    state.help_popup.help_scroll_offset = target_line;
    terminal
        .draw(|frame| {
            render::render(
                frame,
                &mut state,
                &mut textarea,
                None,
                None,
                &ylva::ToolRegistry::new(),
            );
        })
        .expect("scrolled help popup draw");

    let content_area = state.layout.help_popup.content_area;
    assert_eq!(
        state.help_popup.help_scroll_offset, target_line,
        "the target heading should be visible at the top of the scrolled popup"
    );
    let target_column_end = content_area.x + "Help popup".chars().count() as u16;
    let target_row = content_area.y;
    let textarea_context = TextareaContext {
        is_empty: true,
        text: String::new(),
        cursor_row: 0,
        cursor_row_is_last: true,
    };

    presenter::update(
        &mut state,
        TuiEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: content_area.x,
            row: target_row,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }),
        &textarea_context,
    );
    presenter::update(
        &mut state,
        TuiEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: target_column_end,
            row: target_row,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }),
        &textarea_context,
    );
    let effects = presenter::update(
        &mut state,
        TuiEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: target_column_end,
            row: target_row,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }),
        &textarea_context,
    );

    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::CopyToClipboard(text) if text == "Help popup")),
        "dragging a scrolled help line should copy its rendered text: {effects:?}"
    );
}
