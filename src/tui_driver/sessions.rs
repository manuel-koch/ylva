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

//! Asynchronous, identity-scoped session discovery and replacement preparation.

use std::path::PathBuf;

use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use uuid::Uuid;

use crate::agent::{AgentCommand, AgentLoop, UiUpdate};
use crate::agent_factory::{select_root_agent_type, AgentFactory};
use crate::config::ProfileConfig;
use crate::store::{ChildRunRecord, SessionState, Store};
use crate::tui_reducer_state::{
    AppState, SessionAction, SessionChoice, SessionPicker, SessionPickerPhase, SlashOverlay,
};

const SESSION_PICKER_LIMIT: usize = 32;

pub(super) struct PreparedSession {
    pub agent: AgentLoop,
    pub updates: mpsc::Receiver<UiUpdate>,
    pub transcript: SessionState,
    pub child_runs: Vec<ChildRunRecord>,
    pub input_history: Vec<String>,
}

pub(super) enum SessionResult {
    Listed {
        session_id: Uuid,
        picker_id: Uuid,
        result: anyhow::Result<(Vec<SessionChoice>, Vec<String>)>,
    },
    Prepared {
        session_id: Uuid,
        picker_id: Uuid,
        created_clone: Option<Uuid>,
        result: anyhow::Result<Box<PreparedSession>>,
    },
}

pub(super) struct SessionTasks {
    pub sender: mpsc::UnboundedSender<SessionResult>,
    pub receiver: mpsc::UnboundedReceiver<SessionResult>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    cancellation: Option<CancellationToken>,
}

impl SessionTasks {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self {
            sender,
            receiver,
            tasks: Vec::new(),
            cancellation: None,
        }
    }

    pub fn cancel(&mut self) {
        if let Some(cancellation) = self.cancellation.take() {
            cancellation.cancel();
        }
    }

    pub fn list(&mut self, state: &mut AppState, store: Option<Store>) {
        self.cancel();
        let picker = SessionPicker::loading();
        let picker_id = picker.identity;
        state.slash_overlay = SlashOverlay::SessionPicker(picker);
        let Some(store) = store else {
            selection_error(
                state,
                picker_id,
                "Session persistence is unavailable.".into(),
            );
            return;
        };
        let session_id = store.session_id();
        let chats_root = store.chats_root().to_path_buf();
        let project_key = store.project_key().to_owned();
        let sender = self.sender.clone();
        let span = store.span();
        self.tasks.push(tokio::spawn(
            async move {
                let result = tokio::task::spawn_blocking(move || {
                    list_candidates(chats_root, project_key, session_id)
                })
                .await
                .map_err(anyhow::Error::from)
                .and_then(|result| result);
                let _ = sender.send(SessionResult::Listed {
                    session_id,
                    picker_id,
                    result,
                });
            }
            .instrument(span),
        ));
    }

    pub fn prepare(
        &mut self,
        state: &mut AppState,
        source_id: Uuid,
        action: SessionAction,
        agent: &AgentLoop,
        driver: Option<&super::TitleDriver>,
    ) {
        let SlashOverlay::SessionPicker(picker) = &state.slash_overlay else {
            return;
        };
        if picker.phase != SessionPickerPhase::Ready {
            return;
        }
        let picker_id = picker.identity;
        let Some(driver) = driver else {
            selection_error(
                state,
                picker_id,
                "Session replacement is unavailable in this host.".into(),
            );
            return;
        };
        let Some(store) = agent.store() else {
            selection_error(
                state,
                picker_id,
                "Session persistence is unavailable.".into(),
            );
            return;
        };
        let cwd = match std::env::current_dir() {
            Ok(cwd) => cwd,
            Err(error) => {
                selection_error(
                    state,
                    picker_id,
                    format!("Cannot resolve launch directory: {error}"),
                );
                return;
            }
        };
        let session_id = store.session_id();
        let chats_root = store.chats_root().to_path_buf();
        let factory = driver.factory.clone();
        let profile = driver.profile.clone();
        let command_sender = agent.command_sender();
        let reply = match request_barrier(&command_sender) {
            Ok(reply) => reply,
            Err(error) => {
                selection_error(state, picker_id, error.to_string());
                return;
            }
        };
        let cancellation = CancellationToken::new();
        self.cancellation = Some(cancellation.clone());
        if let SlashOverlay::SessionPicker(picker) = &mut state.slash_overlay {
            picker.action = action;
            picker.phase = SessionPickerPhase::Preparing;
            picker.error = None;
        }
        let sender = self.sender.clone();
        let span = store.span();
        tracing::info!(%session_id, %picker_id, %source_id, action = action.label(), "session preparation started");
        self.tasks.push(tokio::spawn(async move {
            let mut created_clone = None;
            let result = async {
                anyhow::ensure!(
                    reply.await?,
                    "Cannot switch sessions while root, subagent, or compaction work remains."
                );
                anyhow::ensure!(!cancellation.is_cancelled(), "Session selection cancelled.");
                let prepare_cancellation = cancellation.clone();
                let (prepared, clone_id) = tokio::task::spawn_blocking(move || {
                    prepare_target(
                        &factory,
                        &profile,
                        chats_root,
                        cwd,
                        source_id,
                        action,
                        &prepare_cancellation,
                    )
                })
                .await?;
                created_clone = clone_id;
                let prepared = prepared?;
                anyhow::ensure!(!cancellation.is_cancelled(), "Session selection cancelled.");
                let reply = request_barrier(&command_sender)?;
                anyhow::ensure!(reply.await?, "Session became busy before activation.");
                anyhow::ensure!(!cancellation.is_cancelled(), "Session selection cancelled.");
                Ok(Box::new(prepared))
            }
            .await;
            if let Err(error) = &result {
                tracing::warn!(%session_id, %picker_id, %source_id, action = action.label(),
                    %error, created_clone = ?created_clone, "session preparation did not activate a target");
            }
            let _ = sender.send(SessionResult::Prepared {
                session_id,
                picker_id,
                created_clone,
                result,
            });
        }.instrument(span)));
    }

    pub async fn finish(&mut self) {
        self.cancel();
        for task in self.tasks.drain(..) {
            if let Err(error) = task.await {
                tracing::error!(%error, "session preparation task failed");
            }
        }
    }
}

fn request_barrier(
    command_sender: &mpsc::Sender<AgentCommand>,
) -> anyhow::Result<oneshot::Receiver<bool>> {
    let (reply, receiver) = oneshot::channel();
    command_sender.try_send(AgentCommand::PrepareSessionSwitch {
        reply: reply.into(),
    })?;
    Ok(receiver)
}

pub(super) fn list_candidates(
    chats_root: PathBuf,
    project_key: String,
    current_session: Uuid,
) -> anyhow::Result<(Vec<SessionChoice>, Vec<String>)> {
    let (sessions, diagnostics) = Store::list_sessions_with_diagnostics(&chats_root, &project_key)?;
    let choices = sessions
        .into_iter()
        .filter(|session| session.parent_session.is_none() && session.session_id != current_session)
        .take(SESSION_PICKER_LIMIT)
        .map(|session| {
            let title = crate::app::single_line(&session.title).trim().to_string();
            let last_used_at = chrono::DateTime::parse_from_rfc3339(&session.last_used_at)?
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S %:z")
                .to_string();
            Ok(SessionChoice {
                session_id: session.session_id,
                title: if title.is_empty() {
                    "Untitled".into()
                } else {
                    title
                },
                last_used_at,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok((choices, diagnostics))
}

pub(super) fn prepare_target(
    factory: &AgentFactory,
    profile: &ProfileConfig,
    chats_root: PathBuf,
    cwd: PathBuf,
    source_id: Uuid,
    action: SessionAction,
    cancellation: &CancellationToken,
) -> (anyhow::Result<PreparedSession>, Option<Uuid>) {
    let mut created_clone = None;
    let result = (|| {
        let path =
            crate::store::session_path(&chats_root, &crate::store::project_key(&cwd), source_id);
        let meta = Store::read_session_meta(&path)?
            .ok_or_else(|| anyhow::anyhow!("Session {source_id} is missing or invalid."))?;
        anyhow::ensure!(
            meta.parent_session.is_none(),
            "Only root sessions can be selected."
        );
        let stored_type = Store::read_latest_agent_type(&path)?;
        let selection = select_root_agent_type(None, stored_type.as_deref(), true, profile)
            .map_err(anyhow::Error::msg)?;
        anyhow::ensure!(!cancellation.is_cancelled(), "Session selection cancelled.");
        let store = match action {
            SessionAction::Resume => Store::resume(&chats_root, &cwd, source_id)?,
            SessionAction::Clone => {
                let store = Store::clone_session(
                    &chats_root,
                    &cwd,
                    source_id,
                    Some(&selection.agent_type),
                )?;
                created_clone = Some(store.session_id());
                store
            }
        };
        let meta = Store::read_session_meta(store.path())?
            .ok_or_else(|| anyhow::anyhow!("Opened session has no valid metadata."))?;
        anyhow::ensure!(
            meta.parent_session.is_none(),
            "Only root sessions can be selected."
        );
        let stored_type = Store::read_latest_agent_type(store.path())?;
        let selection = select_root_agent_type(None, stored_type.as_deref(), true, profile)
            .map_err(anyhow::Error::msg)?;
        anyhow::ensure!(!cancellation.is_cancelled(), "Session selection cancelled.");
        let transcript = Store::read_session_state(store.path())?;
        let child_runs = Store::read_child_runs(store.path())?;
        let input_history = store.read_past_explicit_user_messages(profile.input_history_limit)?;
        let (agent, updates) =
            factory.create_root_for_store(profile, &selection.agent_type, store)?;
        anyhow::ensure!(!cancellation.is_cancelled(), "Session selection cancelled.");
        Ok(PreparedSession {
            agent,
            updates,
            transcript,
            child_runs,
            input_history,
        })
    })();
    (result, created_clone)
}

pub(super) fn picker_mut(state: &mut AppState, picker_id: Uuid) -> Option<&mut SessionPicker> {
    if let SlashOverlay::SessionPicker(picker) = &mut state.slash_overlay {
        if picker.identity == picker_id {
            return Some(picker);
        }
    }
    if let Some(snapshot) = &mut state.pending_input_snapshot {
        if let SlashOverlay::SessionPicker(picker) = &mut snapshot.slash_overlay {
            if picker.identity == picker_id {
                return Some(picker);
            }
        }
    }
    None
}

pub(super) fn selection_error(state: &mut AppState, picker_id: Uuid, error: String) {
    tracing::warn!(%picker_id, %error, "session selection failed");
    if let Some(picker) = picker_mut(state, picker_id) {
        picker.phase = SessionPickerPhase::Ready;
        picker.error = Some(format!("{}: {error}", picker.action.label()));
    }
}
