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

fn render_offline(state: &mut ylva::AppState, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let mut textarea = ratatui_textarea::TextArea::default();
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
        .unwrap();
    terminal.backend().buffer().clone()
}

fn completed_render_fixture(content: String) -> ChatEntry {
    ChatEntry::ToolBubble {
        id: uuid::Uuid::new_v4(),
        name: "read".into(),
        collapsed: false,
        state: Box::new(ylva::ToolCallState::Done {
            arguments: serde_json::json!({}),
            title_arguments: vec![(
                "path".into(),
                serde_json::json!("資料/a long wrapped file name with Unicode.rs"),
            )],
            content_sections: vec![],
            result_section: ylva::tui_reducer_state::ResultSection::plain(content),
            summary: vec![],
            duration: Duration::from_secs(1),
        }),
    }
}

fn present_render_event(state: &mut ylva::AppState, event: ylva::DomainEvent) {
    ylva::presenter::update(
        state,
        ylva::TuiEvent::Domain(event),
        &empty_textarea_context(),
    );
}

async fn wait_for_frame(
    last_buffer: &Arc<std::sync::Mutex<Buffer>>,
    predicate: impl Fn(&Buffer) -> bool,
) -> Buffer {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let buffer = last_buffer.lock().expect("buffer snapshot lock").clone();
            if predicate(&buffer) {
                return buffer;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "expected frame did not render:\n{}",
            rendered_text(&last_buffer.lock().unwrap())
        )
    })
}

struct GatedProvider {
    request_started: tokio::sync::Semaphore,
    release_request: tokio::sync::Semaphore,
    release_response: Arc<tokio::sync::Semaphore>,
    requests: AtomicUsize,
}

impl ylva::Provider for GatedProvider {
    fn name(&self) -> &str {
        "gated"
    }

    fn list_models(&self) -> futures::future::BoxFuture<'_, anyhow::Result<Vec<ModelInfo>>> {
        Box::pin(async { Ok(vec![]) })
    }

    fn chat<'a>(
        &'a self,
        _messages: Vec<Message>,
        _tools: Vec<ToolSchema>,
        cancel: CancellationToken,
    ) -> futures::future::BoxFuture<'a, anyhow::Result<mpsc::Receiver<StreamEvent>>> {
        Box::pin(async move {
            let request = self.requests.fetch_add(1, Ordering::SeqCst) + 1;
            self.request_started.add_permits(1);
            self.release_request.acquire().await?.forget();
            let (sender, receiver) = mpsc::channel(4);
            sender
                .send(StreamEvent::Text(format!("answer-{request}")))
                .await?;
            let release_response = Arc::clone(&self.release_response);
            tokio::spawn(async move {
                let event = tokio::select! {
                    permit = release_response.acquire() => {
                        permit.expect("response gate open").forget();
                        StreamEvent::Done
                    }
                    _ = cancel.cancelled() => StreamEvent::Cancelled,
                };
                let _ = sender.send(event).await;
            });
            Ok(receiver)
        })
    }
}

struct GatedTuiRun {
    actions: mpsc::Sender<AppAction>,
    last_buffer: Arc<std::sync::Mutex<Buffer>>,
    provider: Arc<GatedProvider>,
    handle: tokio::task::JoinHandle<ylva::TuiState>,
}

impl GatedTuiRun {
    fn new(initial_prompt: Option<String>) -> Self {
        let provider = Arc::new(GatedProvider {
            request_started: tokio::sync::Semaphore::new(0),
            release_request: tokio::sync::Semaphore::new(0),
            release_response: Arc::new(tokio::sync::Semaphore::new(0)),
            requests: AtomicUsize::new(0),
        });
        let (backend, last_buffer, _) = InspectableTestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let (actions, action_receiver) = mpsc::channel(64);
        let engine = Arc::new(EngineImpl::new(
            Arc::clone(&provider) as Arc<dyn ylva::Provider>,
            common::test_engine_config(),
        ));
        let (agent, updates) = AgentLoop::new(engine, Arc::new(ToolRegistry::new()));
        let handle = tokio::spawn(async move {
            tui_driver::run_tui(
                &mut terminal,
                agent,
                updates,
                initial_prompt,
                action_receiver,
                &mut NoopClipboard,
            )
            .await
            .expect("run_tui completed")
        });
        Self {
            actions,
            last_buffer,
            provider,
            handle,
        }
    }

    async fn wait_for_request(&self) {
        tokio::time::timeout(
            Duration::from_secs(5),
            self.provider.request_started.acquire(),
        )
        .await
        .expect("provider request started")
        .expect("request gate open")
        .forget();
    }

    async fn start_response(&self, request: usize) {
        self.wait_for_request().await;
        self.provider.release_request.add_permits(1);
        self.wait_for_text(&format!("answer-{request}")).await;
    }

    async fn wait_for_text(&self, expected: &str) {
        wait_for_frame(&self.last_buffer, |buffer| {
            rendered_text(buffer).contains(expected)
        })
        .await;
    }

    async fn finish_response(&self, request: usize) {
        self.provider.release_response.add_permits(1);
        wait_for_frame(&self.last_buffer, |buffer| {
            let text = rendered_text(buffer);
            text.contains(&format!("answer-{request}")) && text.contains("Idle")
        })
        .await;
    }

    async fn close(self) -> ylva::TuiState {
        drop(self.actions);
        tokio::time::timeout(Duration::from_secs(5), self.handle)
            .await
            .expect("TUI shutdown completed")
            .expect("TUI task joined")
    }

    async fn wait_for_exit(self) -> ylva::TuiState {
        let state = tokio::time::timeout(Duration::from_secs(5), self.handle)
            .await
            .expect("TUI exited after cancellation")
            .expect("TUI task joined");
        drop(self.actions);
        state
    }
}

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
    frame_history: Option<Arc<std::sync::Mutex<Vec<Buffer>>>>,
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
                frame_history: None,
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
        self.backend.flush()?;
        if let Some(history) = &self.frame_history {
            history
                .lock()
                .expect("frame history lock")
                .push(self.backend.buffer().clone());
        }
        Ok(())
    }
}

fn model_picker_choices(choice_count: usize) -> Vec<ylva::tui_reducer_state::ModelChoice> {
    (0..choice_count)
        .map(|index| ylva::tui_reducer_state::ModelChoice {
            label: ylva::tui_reducer_state::ChoiceLabel {
                identity: format!("stub/model-{index}"),
                details: Vec::new(),
            },
            initial_reasoning_effort: None,
            selection: ylva::ProviderModelOption {
                context_length_override: None,
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

fn is_assistant_message(entry: &ChatEntry) -> bool {
    matches!(entry, ChatEntry::AssistantMessage { .. })
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

#[path = "tui_render_state/bubbles.rs"]
mod bubbles;
#[path = "tui_render_state/cache.rs"]
mod cache;
#[path = "tui_render_state/overlays.rs"]
mod overlays;
#[path = "tui_render_state/roundtrip.rs"]
mod roundtrip;
#[path = "tui_render_state/selection.rs"]
mod selection;
#[path = "tui_render_state/subagent_views.rs"]
mod subagent_views;
