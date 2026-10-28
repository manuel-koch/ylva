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

use super::super::sessions::{list_candidates, picker_mut, prepare_target, selection_error};
use super::*;
use crate::store::{SessionMeta, Store, StoreEvent};
use crate::tui_reducer_state::{SessionAction, SessionPicker, SessionPickerPhase};
use ratatui::backend::{Backend, ClearType, TestBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use std::path::Path;
use tokio_util::sync::CancellationToken;

struct SessionTestBackend {
    backend: TestBackend,
    frames: mpsc::UnboundedSender<String>,
}

impl Backend for SessionTestBackend {
    type Error = std::convert::Infallible;
    fn draw<'a, I>(&mut self, cells: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.backend.draw(cells)
    }
    fn append_lines(&mut self, count: u16) -> Result<(), Self::Error> {
        self.backend.append_lines(count)
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
    fn clear_region(&mut self, region: ClearType) -> Result<(), Self::Error> {
        self.backend.clear_region(region)
    }
    fn size(&self) -> Result<Size, Self::Error> {
        self.backend.size()
    }
    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        self.backend.window_size()
    }
    fn flush(&mut self) -> Result<(), Self::Error> {
        let buffer = self.backend.buffer();
        let text = buffer
            .content
            .chunks(usize::from(buffer.area.width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        let _ = self.frames.send(text);
        self.backend.flush()
    }
}

async fn wait_for_frame(frames: &mut mpsc::UnboundedReceiver<String>, text: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(frame) = frames.recv().await {
            if frame.contains(text) {
                return;
            }
        }
        panic!("frame channel closed before {text}");
    })
    .await
    .unwrap_or_else(|_| panic!("no frame containing {text}"));
}

async fn submit(actions: &mpsc::Sender<AppAction>, text: &str) {
    actions.send(AppAction::Paste(text.into())).await.unwrap();
    actions
        .send(AppAction::KeyInput(key_event(KeyCode::Enter)))
        .await
        .unwrap();
}

struct PausedConfiguredCopy {
    engine: Arc<dyn Engine>,
    created: Arc<tokio::sync::Notify>,
    resume: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl Engine for PausedConfiguredCopy {
    fn configured_copy(&self) -> anyhow::Result<Arc<dyn Engine>> {
        self.created.notify_one();
        self.resume.lock().unwrap().recv()?;
        self.engine.configured_copy()
    }
    fn chat<'a>(
        &'a self,
        messages: Vec<crate::Message>,
        tools: Vec<crate::ToolSchema>,
        cancel: CancellationToken,
    ) -> futures::future::BoxFuture<'a, anyhow::Result<mpsc::Receiver<crate::StreamEvent>>> {
        self.engine.chat(messages, tools, cancel)
    }
    fn config(&self) -> EngineConfig {
        self.engine.config()
    }
    fn list_models(&self) -> futures::future::BoxFuture<'_, anyhow::Result<Vec<crate::ModelInfo>>> {
        self.engine.list_models()
    }
    fn switch_model(&self, model: &str) -> anyhow::Result<()> {
        self.engine.switch_model(model)
    }
}

fn write_session(
    chats_root: &Path,
    cwd: &Path,
    identity: Uuid,
    created_at: &str,
    parent_session: Option<Uuid>,
    agent_type: Option<&str>,
) -> std::path::PathBuf {
    let project_key = crate::store::project_key(cwd);
    let path = crate::store::session_path(chats_root, &project_key, identity);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let meta = SessionMeta {
        version: 1,
        schema_version: 1,
        app_version: "test".into(),
        project_key,
        created_at: created_at.into(),
        provider_connection: "mock".into(),
        model: "configured".into(),
        session_id: identity,
        title: String::new(),
        parent_session,
        agent_type: agent_type.map(str::to_owned),
        cloned_from: None,
    };
    let events = [
        StoreEvent::Meta(meta),
        StoreEvent::Message {
            version: 1,
            timestamp: created_at.into(),
            message: crate::Message::User {
                content: "historical task".into(),
                kind: crate::UserMessageKind::Explicit,
            },
        },
    ];
    let content = events
        .iter()
        .map(|event| serde_json::to_string(event).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(&path, content).unwrap();
    path
}

fn session_factory() -> AgentFactory {
    let config = EngineConfig {
        model: "configured".into(),
        ..StubEngine::with_models(&[], "unused").config()
    };
    let engine: Arc<dyn Engine> = Arc::new(EngineImpl::new(
        Arc::new(crate::MockProvider::new(
            vec!["finished answer".into()],
            Duration::ZERO,
        )),
        config,
    ));
    AgentFactory::new(HashMap::from([("default".into(), engine)])).with_allowed_tools(&[])
}

#[test]
fn picker_results_reach_parked_approval_input_but_ignore_dismissed_or_reopened_pickers() {
    let mut state = AppState::new();
    let mut picker = SessionPicker::loading();
    picker.action = SessionAction::Clone;
    let identity = picker.identity;
    state.parked_slash_overlay = Some(SlashOverlay::SessionPicker(picker));
    let picker = picker_mut(&mut state, identity).unwrap();
    picker
        .choices
        .push(crate::tui_reducer_state::SessionChoice {
            session_id: Uuid::new_v4(),
            title: "Loaded while approving".into(),
            last_used_at: "date".into(),
        });
    picker.phase = SessionPickerPhase::Ready;
    selection_error(&mut state, identity, "selection failure".into());
    state.slash_overlay = state.parked_slash_overlay.take().unwrap();
    let picker = picker_mut(&mut state, identity).unwrap();
    assert_eq!(picker.action, SessionAction::Clone);
    assert_eq!(picker.choices.len(), 1);
    assert_eq!(picker.error.as_deref(), Some("Clone: selection failure"));
    state.slash_overlay = SlashOverlay::Hidden;
    assert!(picker_mut(&mut state, identity).is_none());
    state.slash_overlay = SlashOverlay::SessionPicker(SessionPicker::loading());
    assert!(picker_mut(&mut state, identity).is_none());
}

#[test]
fn session_listing_filters_before_fixed_limit_and_uses_instant_uuid_order() {
    let directory = tempfile::tempdir().unwrap();
    let current_id = Uuid::from_u128(200);
    for index in 1..=36 {
        write_session(
            directory.path(),
            directory.path(),
            Uuid::from_u128(index),
            "2026-01-01T00:00:00Z",
            None,
            None,
        );
    }
    for index in 100..=150 {
        write_session(
            directory.path(),
            directory.path(),
            Uuid::from_u128(index),
            "2026-02-01T00:00:00Z",
            Some(current_id),
            None,
        );
    }
    write_session(
        directory.path(),
        directory.path(),
        current_id,
        "2026-03-01T00:00:00Z",
        None,
        None,
    );
    let latest_id = Uuid::from_u128(1);
    let latest_path = write_session(
        directory.path(),
        directory.path(),
        latest_id,
        "2026-01-01T01:30:00+01:00",
        None,
        None,
    );
    let mut meta = Store::read_session_meta(&latest_path).unwrap().unwrap();
    meta.title = "Duplicate\n\u{1b}Title".into();
    let content = serde_json::to_string(&StoreEvent::Meta(meta)).unwrap() + "\n";
    std::fs::write(&latest_path, &content).unwrap();
    let project_key = crate::store::project_key(directory.path());
    let report = Store::maintain_sessions(
        directory.path(),
        &project_key,
        &crate::store::SessionSelection::Ids(vec![latest_id]),
        crate::store::MaintenanceAction::Compress,
        false,
    )
    .unwrap();
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    let (choices, diagnostics) =
        list_candidates(directory.path().into(), project_key, current_id).unwrap();
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(choices.len(), 32);
    assert_eq!(choices[0].session_id, latest_id);
    assert_eq!(choices[1].session_id, Uuid::from_u128(36));
    assert_eq!(choices.last().unwrap().session_id, Uuid::from_u128(6));
    assert!(!choices[0].title.contains(['\n', '\u{1b}']));
    assert_eq!(choices[1].title, "Untitled");
    assert!(choices[0].last_used_at.contains(['+', '-']));
}

#[test]
fn prepared_resume_and_clone_use_configured_engines_and_never_duplicate_messages() {
    crate::test_support::run_locked(async {
        for action in [SessionAction::Resume, SessionAction::Clone] {
            let directory = tempfile::tempdir().unwrap();
            let source_id = Uuid::new_v4();
            let path = write_session(
                directory.path(),
                directory.path(),
                source_id,
                "2026-01-01T00:00:00Z",
                None,
                None,
            );
            let original = std::fs::read(&path).unwrap();
            let factory = session_factory();
            let (prepared, clone_id) = prepare_target(
                &factory,
                &ProfileConfig::default(),
                directory.path().into(),
                directory.path().into(),
                source_id,
                action,
                &CancellationToken::new(),
            );
            let mut prepared = prepared.unwrap();
            assert_eq!(prepared.agent.engine().config().model, "configured");
            assert_eq!(prepared.transcript.messages.len(), 1);
            let target = prepared.agent.store().unwrap();
            assert_eq!(
                target.session_id() == source_id,
                action == SessionAction::Resume
            );
            assert_eq!(
                clone_id,
                if action == SessionAction::Clone {
                    Some(target.session_id())
                } else {
                    None
                }
            );
            if action == SessionAction::Clone {
                assert_eq!(
                    Store::read_session_meta(target.path())
                        .unwrap()
                        .unwrap()
                        .cloned_from,
                    Some(source_id)
                );
                assert_eq!(std::fs::read(&path).unwrap(), original);
            }
            let command_sender = prepared.agent.command_sender();
            let handle = prepared.agent.run();
            command_sender
                .send(AgentCommand::Prompt("new task".into()))
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                while let Some(update) = prepared.updates.recv().await {
                    if matches!(update, UiUpdate::StreamDone) {
                        break;
                    }
                }
            })
            .await
            .unwrap();
            target.flush().await;
            assert_eq!(Store::read_messages(target.path()).unwrap().len(), 3);
            if action == SessionAction::Clone {
                assert_eq!(std::fs::read(&path).unwrap(), original);
            }
            drop(command_sender);
            drop(prepared.agent);
            drop(handle.await.unwrap().unwrap());
        }
    });
}

#[test]
fn failed_source_validation_and_precreation_cancellation_never_create_clones() {
    crate::test_support::run_locked(async {
        for (parent, agent_type, cancelled) in [
            (Some(Uuid::new_v4()), None, false),
            (None, Some("unavailable"), false),
            (None, None, true),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let source_id = Uuid::new_v4();
            let path = write_session(
                directory.path(),
                directory.path(),
                source_id,
                "2026-01-01T00:00:00Z",
                parent,
                agent_type,
            );
            let cancellation = CancellationToken::new();
            if cancelled {
                cancellation.cancel();
            }
            let (result, clone_id) = prepare_target(
                &session_factory(),
                &ProfileConfig::default(),
                directory.path().into(),
                directory.path().into(),
                source_id,
                SessionAction::Clone,
                &cancellation,
            );
            assert!(result.is_err());
            assert_eq!(clone_id, None);
            assert_eq!(
                Store::list_sessions(
                    directory.path(),
                    &crate::store::project_key(directory.path())
                )
                .unwrap()
                .len(),
                1
            );
            assert!(path.exists());
        }
    });
}

#[test]
fn activation_failure_retains_durable_clone_and_selection_action() {
    crate::test_support::run_locked(async {
        let directory = tempfile::tempdir().unwrap();
        let source_id = Uuid::new_v4();
        write_session(
            directory.path(),
            directory.path(),
            source_id,
            "2026-01-01T00:00:00Z",
            None,
            None,
        );
        let engine: Arc<dyn Engine> = StubEngine::with_models(&[], "configured");
        let factory = AgentFactory::new(HashMap::from([("default".into(), engine)]));
        let (result, clone_id) = prepare_target(
            &factory,
            &ProfileConfig::default(),
            directory.path().into(),
            directory.path().into(),
            source_id,
            SessionAction::Clone,
            &CancellationToken::new(),
        );
        assert!(result.is_err());
        let clone_id = clone_id.expect("durable clone must survive runtime preparation failure");
        let path = crate::store::session_path(
            directory.path(),
            &crate::store::project_key(directory.path()),
            clone_id,
        );
        assert_eq!(Store::read_messages(&path).unwrap().len(), 1);
        let mut state = AppState::new();
        let mut picker = SessionPicker::loading();
        picker.action = SessionAction::Clone;
        let picker_id = picker.identity;
        state.slash_overlay = SlashOverlay::SessionPicker(picker);
        selection_error(&mut state, picker_id, "failure".into());
        let SlashOverlay::SessionPicker(picker) = &state.slash_overlay else {
            panic!()
        };
        assert_eq!(picker.action, SessionAction::Clone);
        assert_eq!(picker.phase, SessionPickerPhase::Ready);
        assert_eq!(picker.error.as_deref(), Some("Clone: failure"));
        let notices = Arc::default();
        report_unactivated_clone(&mut state, &notices, clone_id);
        assert_eq!(*notices.lock().unwrap(), vec![clone_id]);
    });
}

#[test]
fn locked_missing_corrupt_and_archived_sources_have_explicit_local_outcomes() {
    crate::test_support::run_locked(async {
        for action in [SessionAction::Resume, SessionAction::Clone] {
            let directory = tempfile::tempdir().unwrap();
            let identity = Uuid::new_v4();
            let path = write_session(
                directory.path(),
                directory.path(),
                identity,
                "2026-01-01T00:00:00Z",
                None,
                None,
            );
            let held = Store::resume(directory.path(), directory.path(), identity).unwrap();
            let (result, clone_id) = prepare_target(
                &session_factory(),
                &ProfileConfig::default(),
                directory.path().into(),
                directory.path().into(),
                identity,
                action,
                &CancellationToken::new(),
            );
            assert!(matches!(
                result.err().unwrap().downcast_ref::<crate::StoreError>(),
                Some(crate::StoreError::SessionLocked { .. })
            ));
            assert!(clone_id.is_none());
            drop(held);
            let unlocked = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    match Store::resume(directory.path(), directory.path(), identity) {
                        Ok(store) => break store,
                        Err(crate::StoreError::SessionLocked { .. }) => {
                            tokio::task::yield_now().await
                        }
                        Err(error) => panic!("{error}"),
                    }
                }
            })
            .await
            .unwrap();
            drop(unlocked);
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let report = Store::maintain_sessions(
                        directory.path(),
                        &crate::project_key(directory.path()),
                        &crate::store::SessionSelection::Ids(vec![identity]),
                        crate::store::MaintenanceAction::Compress,
                        false,
                    )
                    .unwrap();
                    if report.failures.is_empty() {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let archive_path = path.with_extension("jsonl.gz");
            let archive_bytes = std::fs::read(&archive_path).unwrap();
            let (prepared, clone_id) = prepare_target(
                &session_factory(),
                &ProfileConfig::default(),
                directory.path().into(),
                directory.path().into(),
                identity,
                action,
                &CancellationToken::new(),
            );
            let prepared = prepared.unwrap();
            assert_eq!(prepared.transcript.messages.len(), 1);
            if action == SessionAction::Clone {
                assert!(clone_id.is_some());
                assert!(!path.exists());
                assert_eq!(std::fs::read(&archive_path).unwrap(), archive_bytes);
            } else {
                assert!(path.exists());
            }
            drop(prepared);
        }
        for content in [None, Some("corrupt metadata\n")] {
            let directory = tempfile::tempdir().unwrap();
            let identity = Uuid::new_v4();
            let path = crate::session_path(
                directory.path(),
                &crate::project_key(directory.path()),
                identity,
            );
            if let Some(content) = content {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, content).unwrap();
            }
            let (result, clone_id) = prepare_target(
                &session_factory(),
                &ProfileConfig::default(),
                directory.path().into(),
                directory.path().into(),
                identity,
                SessionAction::Clone,
                &CancellationToken::new(),
            );
            assert!(result.is_err());
            assert_eq!(clone_id, None);
        }
    });
}

#[test]
fn picker_preparation_restores_latest_type_for_already_cloned_sources() {
    crate::test_support::run_locked(async {
        let directory = tempfile::tempdir().unwrap();
        let source_id = Uuid::new_v4();
        let path = write_session(
            directory.path(),
            directory.path(),
            source_id,
            "2026-01-01T00:00:00Z",
            None,
            Some("default"),
        );
        let source = Store::clone_session(
            directory.path(),
            directory.path(),
            source_id,
            Some("default"),
        )
        .unwrap();
        source.record_agent_type_changed("reviewer");
        source.flush().await;
        let cloned_source = source.session_id();
        let cloned_path = source.path().to_path_buf();
        drop(source);
        let mut profile = ProfileConfig::default();
        let mut reviewer = profile.agents["default"].clone();
        reviewer.allowed_tools = Some(vec!["read".into()]);
        reviewer.can_spawn = false;
        profile.agents.insert("reviewer".into(), reviewer);
        let result = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let (result, clone_id) = prepare_target(
                    &session_factory(),
                    &profile,
                    directory.path().into(),
                    directory.path().into(),
                    cloned_source,
                    SessionAction::Clone,
                    &CancellationToken::new(),
                );
                match result {
                    Ok(prepared) => break (prepared, clone_id),
                    Err(error)
                        if matches!(
                            error.downcast_ref::<crate::StoreError>(),
                            Some(crate::StoreError::SessionLocked { .. })
                        ) =>
                    {
                        tokio::task::yield_now().await
                    }
                    Err(error) => panic!("{error}"),
                }
            }
        })
        .await
        .unwrap();
        let (prepared, clone_id) = result;
        assert_eq!(prepared.agent.registry().names(), vec!["read"]);
        let store = prepared.agent.store().unwrap();
        assert_eq!(Some(store.session_id()), clone_id);
        assert_eq!(
            Store::read_latest_agent_type(store.path())
                .unwrap()
                .as_deref(),
            Some("reviewer")
        );
        assert_eq!(
            Store::read_session_meta(store.path())
                .unwrap()
                .unwrap()
                .cloned_from,
            Some(cloned_source)
        );
        assert_eq!(Store::read_messages(&path).unwrap().len(), 1);
        assert_eq!(Store::read_messages(&cloned_path).unwrap().len(), 1);
    });
}

#[test]
fn cancelling_after_clone_creation_keeps_clone_visible_and_old_root_usable() {
    crate::test_support::run_locked(async {
        let directory = tempfile::tempdir().unwrap();
        let cwd = std::env::current_dir().unwrap();
        let source_id = Uuid::new_v4();
        let source_path = write_session(
            directory.path(),
            &cwd,
            source_id,
            "2026-01-01T00:00:00Z",
            None,
            None,
        );
        let source_bytes = std::fs::read(&source_path).unwrap();
        let active = Store::create(
            directory.path(),
            &cwd,
            Uuid::new_v4(),
            "mock",
            "configured",
            Some("default"),
        )
        .unwrap();
        active.set_title("Active root".into()).await.unwrap();
        let active_id = active.session_id();
        let active_path = active.path().to_path_buf();
        let created = Arc::new(tokio::sync::Notify::new());
        let (resume_sender, resume_receiver) = std::sync::mpsc::channel();
        let engine: Arc<dyn Engine> = Arc::new(PausedConfiguredCopy {
            engine: session_factory().engine("default").unwrap(),
            created: created.clone(),
            resume: std::sync::Mutex::new(resume_receiver),
        });
        let factory = AgentFactory::new(HashMap::from([("default".into(), engine)]))
            .with_allowed_tools(&[])
            .with_store(active);
        let profile = ProfileConfig::default();
        let (agent, updates) = factory.create(&profile, "default", Role::Root).unwrap();
        let configuration_factory = Arc::new(factory.without_store());
        drop(factory);
        let (title_update_tx, title_update_rx) = mpsc::unbounded_channel();
        let (frame_sender, mut frames) = mpsc::unbounded_channel();
        let mut terminal = Terminal::new(SessionTestBackend {
            backend: TestBackend::new(140, 40),
            frames: frame_sender,
        })
        .unwrap();
        let (actions, action_rx) = mpsc::channel(32);
        let exit_snapshot = crate::app::SessionExitHandle::default();
        let notices = Arc::new(std::sync::Mutex::new(Vec::new()));
        let controller = async {
            submit(&actions, "/session").await;
            wait_for_frame(&mut frames, "enter confirm").await;
            actions
                .send(AppAction::KeyInput(key_event(KeyCode::Right)))
                .await
                .unwrap();
            actions
                .send(AppAction::KeyInput(key_event(KeyCode::Enter)))
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(10), created.notified())
                .await
                .unwrap();
            while frames.try_recv().is_ok() {}
            actions.send(AppAction::Escape).await.unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                while let Some(frame) = frames.recv().await {
                    if !frame.contains("Sessions") {
                        break;
                    }
                }
            })
            .await
            .unwrap();
            resume_sender.send(()).unwrap();
            wait_for_frame(&mut frames, "Clone created but not activated").await;
            submit(&actions, "still active after cancellation").await;
            wait_for_frame(&mut frames, "finished answer").await;
            actions.send(AppAction::CtrlC).await.unwrap();
            actions.send(AppAction::CtrlC).await.unwrap();
        };
        let mut clipboard = NoopClipboard;
        let run = run_tui_with_title(
            &mut terminal,
            agent,
            updates,
            None,
            action_rx,
            &mut clipboard,
            TuiRuntime {
                driver: Some(TitleDriver {
                    factory: configuration_factory,
                    profile,
                    attempt: TitleAttempt::default(),
                    title_update_tx,
                }),
                update_rx: title_update_rx,
                display_metadata: TuiDisplayMetadata::default(),
                input_history_limit: 50,
                tui_config: Default::default(),
                exit_snapshot: Some(exit_snapshot.clone()),
                unactivated_clones: notices.clone(),
                startup_notice: None,
                ..TuiRuntime::disabled()
            },
        );
        let (state, ()) = tokio::join!(run, controller);
        let (state, _outcome) = state.unwrap();
        assert_eq!(
            exit_snapshot.lock().unwrap().as_ref().unwrap().session_id,
            active_id
        );
        let clone_id = notices.lock().unwrap()[0];
        let clone_path = crate::session_path(directory.path(), &crate::project_key(&cwd), clone_id);
        assert_eq!(Store::read_messages(&clone_path).unwrap().len(), 1);
        assert_eq!(Store::read_messages(&active_path).unwrap().len(), 2);
        assert_eq!(std::fs::read(source_path).unwrap(), source_bytes);
        assert!(state.chat_entries.iter().any(|entry| matches!(entry, ChatEntry::Notice { text } if text.contains(&clone_id.to_string()))));
    });
}

#[test]
fn assembled_session_picker_switches_resume_and_clone_and_exit_identity() {
    crate::test_support::run_locked(async {
        for (action, return_to_original) in [
            (SessionAction::Resume, false),
            (SessionAction::Clone, false),
            (SessionAction::Resume, true),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let cwd = std::env::current_dir().unwrap();
            let source_id = Uuid::new_v4();
            let source_path = write_session(
                directory.path(),
                &cwd,
                source_id,
                "2026-01-01T00:00:00Z",
                None,
                None,
            );
            let source_bytes = std::fs::read(&source_path).unwrap();
            let original_store = Store::create(
                directory.path(),
                &cwd,
                Uuid::new_v4(),
                "mock",
                "configured",
                Some("default"),
            )
            .unwrap();
            let original_id = original_store.session_id();
            let original_path = original_store.path().to_path_buf();
            if return_to_original {
                original_store.record_message(&crate::Message::User {
                    content: "original identity task".into(),
                    kind: crate::UserMessageKind::Explicit,
                });
                original_store.flush().await;
            }
            let factory = session_factory().with_store(original_store);
            let profile = ProfileConfig::default();
            let (agent, updates) = factory.create(&profile, "default", Role::Root).unwrap();
            let configuration_factory = Arc::new(factory.without_store());
            drop(factory);
            let (title_update_tx, title_update_rx) = mpsc::unbounded_channel();
            let exit_snapshot = crate::app::SessionExitHandle::default();
            let (frames_sender, mut frames) = mpsc::unbounded_channel();
            let mut terminal = Terminal::new(SessionTestBackend {
                backend: TestBackend::new(140, 40),
                frames: frames_sender,
            })
            .unwrap();
            let (actions, action_rx) = mpsc::channel(32);
            let controller = async {
                submit(&actions, "/ses").await;
                wait_for_frame(&mut frames, "enter confirm").await;
                if action == SessionAction::Clone {
                    actions
                        .send(AppAction::KeyInput(key_event(KeyCode::Right)))
                        .await
                        .unwrap();
                }

                actions
                    .send(AppAction::KeyInput(key_event(KeyCode::Enter)))
                    .await
                    .unwrap();
                wait_for_frame(&mut frames, "historical task").await;
                submit(&actions, "new task for selected session").await;
                wait_for_frame(&mut frames, "finished answer").await;
                if return_to_original {
                    submit(&actions, "/session").await;
                    wait_for_frame(&mut frames, "enter confirm").await;
                    actions
                        .send(AppAction::KeyInput(key_event(KeyCode::Enter)))
                        .await
                        .unwrap();
                    wait_for_frame(&mut frames, "original identity task").await;
                    submit(&actions, "task after returning to original").await;
                    wait_for_frame(&mut frames, "finished answer").await;
                }
                actions.send(AppAction::CtrlC).await.unwrap();
                actions.send(AppAction::CtrlC).await.unwrap();
            };
            let mut clipboard = NoopClipboard;
            let run = run_tui_with_title(
                &mut terminal,
                agent,
                updates,
                None,
                action_rx,
                &mut clipboard,
                TuiRuntime {
                    driver: Some(TitleDriver {
                        factory: configuration_factory,
                        profile,
                        attempt: TitleAttempt::default(),
                        title_update_tx,
                    }),
                    update_rx: title_update_rx,
                    display_metadata: TuiDisplayMetadata::default(),
                    input_history_limit: 50,
                    tui_config: Default::default(),
                    exit_snapshot: Some(exit_snapshot.clone()),
                    unactivated_clones: Arc::default(),
                    startup_notice: None,
                    ..TuiRuntime::disabled()
                },
            );
            let (state, ()) = tokio::join!(run, controller);
            let (state, _outcome) = state.unwrap();
            assert!(matches!(state.slash_overlay, SlashOverlay::Hidden));
            let active_id = exit_snapshot.lock().unwrap().as_ref().unwrap().session_id;
            assert_eq!(active_id == original_id, return_to_original);
            let selected_id = if return_to_original {
                source_id
            } else {
                active_id
            };
            let target_path = crate::store::session_path(
                directory.path(),
                &crate::store::project_key(&cwd),
                selected_id,
            );
            assert_eq!(Store::read_messages(&target_path).unwrap().len(), 3);
            assert_eq!(
                Store::read_messages(&original_path).unwrap().len(),
                if return_to_original { 3 } else { 0 }
            );
            assert_eq!(selected_id == source_id, action == SessionAction::Resume);
            if action == SessionAction::Clone {
                assert_eq!(std::fs::read(source_path).unwrap(), source_bytes);
                assert_eq!(
                    Store::read_session_meta(&target_path)
                        .unwrap()
                        .unwrap()
                        .cloned_from,
                    Some(source_id)
                );
            }
            let reopened = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    match Store::resume(directory.path(), &cwd, original_id) {
                        Ok(store) => break store,
                        Err(crate::StoreError::SessionLocked { .. }) => {
                            tokio::task::yield_now().await
                        }
                        Err(error) => panic!("original session cannot reopen: {error}"),
                    }
                }
            })
            .await
            .unwrap();
            drop(reopened);
        }
    });
}

#[test]
fn queued_continuous_mouse_motion_is_drawn_as_one_frame() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

    for kind in [
        MouseEventKind::ScrollUp,
        MouseEventKind::Drag(MouseButton::Left),
    ] {
        crate::test_support::run_locked(async {
            let (agent, updates) = session_factory()
                .create(&ProfileConfig::default(), "default", Role::Root)
                .unwrap();
            let (_title_update_tx, title_update_rx) = mpsc::unbounded_channel();
            let (frame_sender, mut frames) = mpsc::unbounded_channel();
            let mut terminal = Terminal::new(SessionTestBackend {
                backend: TestBackend::new(60, 30),
                frames: frame_sender,
            })
            .unwrap();
            let (actions, action_rx) = mpsc::channel(64);
            let draft = (0..200)
                .map(|line| format!("line {line}"))
                .collect::<Vec<_>>()
                .join("\n");
            actions.send(AppAction::Paste(draft)).await.unwrap();
            // Row of the one-line collapsed draft in the 30-row terminal.
            let wheel_row = 26;
            for _ in 0..40 {
                actions
                    .send(AppAction::MouseInput(MouseEvent {
                        kind,
                        column: 5,
                        row: wheel_row,
                        modifiers: KeyModifiers::NONE,
                    }))
                    .await
                    .unwrap();
            }
            actions.send(AppAction::CtrlC).await.unwrap();
            actions.send(AppAction::CtrlC).await.unwrap();
            let mut clipboard = NoopClipboard;
            run_tui_with_title(
                &mut terminal,
                agent,
                updates,
                None,
                action_rx,
                &mut clipboard,
                TuiRuntime {
                    driver: None,
                    update_rx: title_update_rx,
                    display_metadata: TuiDisplayMetadata::default(),
                    input_history_limit: 50,
                    tui_config: Default::default(),
                    exit_snapshot: None,
                    unactivated_clones: Arc::new(std::sync::Mutex::new(Vec::new())),
                    startup_notice: None,
                    ..TuiRuntime::disabled()
                },
            )
            .await
            .unwrap();
            let mut frame_count = 0;
            while frames.try_recv().is_ok() {
                frame_count += 1;
            }
            assert!(frame_count < 10, "drew {frame_count} frames");
        });
    }
}
