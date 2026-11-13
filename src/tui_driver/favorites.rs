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

//! TUI-lifetime owner of favorite-model persistence.
//!
//! The [`FavoriteSaveOwner`] lives in the event loop's locals, outside
//! [`AppState`](crate::tui_reducer_state::AppState), so `/clear`, `/session`
//! replacement, popup closure and approval/clarify snapshot restoration cannot
//! lose it. It holds
//!
//! - the startup-sourced write capability (a [`FavoriteModelsWriter`] captured
//!   from the profile document actually loaded), or the guidance explaining
//!   why persistence is unavailable;
//! - the authoritative favorite membership per provider connection, which is
//!   changed only after a save was confirmed; and
//! - a serialized background save pipeline: at most one blocking write is in
//!   flight, started with `spawn_blocking`; the writer moves into the task and
//!   back out of its result so a retry reuses the advanced source snapshot.
//!
//! Closing the model picker never cancels a started write: the owner outlives
//! every popup, and normal shutdown awaits the write via
//! [`FavoriteSaveOwner::finish`].

use std::collections::HashMap;
use std::sync::Arc;

use tokio::task::JoinHandle;

use super::models::ModelSnapshot;
use super::title::TitleDriver;
use crate::llm::engine::{Engine, ModelSelection, NamedEnginePublication, ReasoningEffortChange};

use crate::config::{
    EngineSaveMode, EngineSaveRequest, FavoriteModelsWriter, FavoriteSaveError,
    FavoriteSaveOutcome, FavoriteSourceLayer, ProfileConfig,
};
use crate::home_relative_path::display_path;
use crate::llm::engine::EngineConfig;
use crate::tui_reducer_state::{AppState, Effect, EngineSaveContext, EngineSaveJob};

#[cfg(test)]
mod tests;

/// Guidance shown when the session has no profile file to save favorites to.
const NO_PROFILE_FILE_GUIDANCE: &str = "favorites cannot be saved: this session is not backed by \
a profile config file (built-in defaults); run `ylva connect <provider>` to create a profile";

/// Guidance shown when the session has no profile file to save engines to.
const NO_PROFILE_FILE_ENGINE_GUIDANCE: &str = "engines cannot be saved: this session is not \
backed by a profile config file (built-in defaults); run `ylva connect <provider>` to create a \
profile";

/// Guidance shown when the save task ended without returning its writer.
const SAVE_TASK_FAILED_GUIDANCE: &str =
    "the favorites save task failed unexpectedly; restart Ylva to save favorites again";

/// Blocking persistence of one favorite membership change.
///
/// Implemented by [`FavoriteModelsWriter`]; tests inject deterministic doubles.
pub(super) trait FavoriteStore: Send + 'static {
    fn set_favorite(
        &mut self,
        connection: &str,
        model_id: &str,
        favorite: bool,
    ) -> Result<FavoriteSaveOutcome, FavoriteSaveError>;

    /// Persist one named-engine create or update.
    fn save_engine(
        &mut self,
        _request: &EngineSaveRequest,
    ) -> Result<FavoriteSaveOutcome, FavoriteSaveError> {
        Err(FavoriteSaveError::Unsupported {
            reason: "this store cannot save engines".to_string(),
        })
    }
}

impl FavoriteStore for FavoriteModelsWriter {
    fn set_favorite(
        &mut self,
        connection: &str,
        model_id: &str,
        favorite: bool,
    ) -> Result<FavoriteSaveOutcome, FavoriteSaveError> {
        FavoriteModelsWriter::set_favorite(self, connection, model_id, favorite)
    }

    fn save_engine(
        &mut self,
        request: &EngineSaveRequest,
    ) -> Result<FavoriteSaveOutcome, FavoriteSaveError> {
        FavoriteModelsWriter::save_engine(self, request)
    }
}

/// Startup-sourced favorites capability handed to [`TuiApp`](super::TuiApp).
///
/// The default value disables persistence.
#[derive(Default)]
pub struct FavoriteSaveSetup {
    writer: Option<FavoriteModelsWriter>,
    /// Configured favorites of every provider connection that has a backing
    /// table in the loaded profile; a connection absent here cannot be saved.
    favorites_by_connection: HashMap<String, Vec<String>>,
    /// Display path and layer of the file saves are written to.
    destination: Option<(String, FavoriteSourceLayer)>,
    /// Engines the loaded profile defines with an `[engines.<name>]` table.
    explicit_engines: HashMap<String, EngineConfig>,
}

impl FavoriteSaveSetup {
    /// Combine the writer captured while loading `profile` with the favorites
    /// the profile's provider-connection tables configure.
    ///
    /// `writer` is `None` for built-in defaults. Environment-only providers
    /// and legacy flat engines have no connection table in `profile`, so their
    /// favorites stay unsavable.
    pub fn from_profile(writer: Option<FavoriteModelsWriter>, profile: &ProfileConfig) -> Self {
        let favorites_by_connection = profile
            .provider_connections
            .iter()
            .map(|(name, connection)| (name.clone(), connection.favorite_models.clone()))
            .collect();
        let destination = writer.as_ref().map(|writer| {
            let (path, layer) = writer.destination();
            (display_path(path), layer)
        });
        Self {
            writer,
            favorites_by_connection,
            destination,
            explicit_engines: profile.engines.clone(),
        }
    }

    /// Validation facts of the engine save form. `runtime_engine_names` is
    /// the complete runtime namespace, explicit and implicit engines.
    pub(super) fn engine_save_context(
        &self,
        runtime_engine_names: impl IntoIterator<Item = String>,
    ) -> EngineSaveContext {
        let mut occupied_names: std::collections::HashSet<String> =
            runtime_engine_names.into_iter().collect();
        occupied_names.extend(self.explicit_engines.keys().cloned());
        EngineSaveContext {
            unavailable: self
                .destination
                .is_none()
                .then(|| NO_PROFILE_FILE_ENGINE_GUIDANCE.to_string()),
            destination: self
                .destination
                .as_ref()
                .map(|(path, _)| path.clone())
                .unwrap_or_default(),
            workspace_layer: self
                .destination
                .as_ref()
                .is_some_and(|(_, layer)| *layer == FavoriteSourceLayer::Workspace),
            persistable_connections: self.favorites_by_connection.keys().cloned().collect(),
            explicit_engines: self.explicit_engines.clone(),
            occupied_names,
        }
    }
}

/// Result of [`FavoriteSaveOwner::request`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FavoriteRequestOutcome {
    /// A background write was started; its completion arrives later.
    Started,
    /// Another write is in flight; the request was dropped.
    IgnoredPending,
    /// Persistence is unavailable for this request; the text explains why.
    Unavailable(String),
}

/// Finished engine save.
#[derive(Debug)]
pub(super) struct EngineSaveCompletion {
    pub(super) job: EngineSaveJob,
    pub(super) result: Result<FavoriteSaveOutcome, FavoriteSaveError>,
}

/// Finished write of either kind, delivered by
/// [`FavoriteSaveOwner::next_completion`].
#[derive(Debug)]
pub(super) enum SaveCompletion {
    Favorite(FavoriteSaveCompletion),
    Engine(Box<EngineSaveCompletion>),
}

impl SaveCompletion {
    #[cfg(test)]
    pub(super) fn favorite(self) -> FavoriteSaveCompletion {
        match self {
            Self::Favorite(completion) => completion,
            Self::Engine(_) => panic!("expected a favorite completion"),
        }
    }
}

/// Finished favorite save.
#[derive(Debug)]
pub(super) struct FavoriteSaveCompletion {
    pub(super) connection: String,
    pub(super) model_id: String,
    /// Desired membership that was requested.
    pub(super) desired: bool,
    pub(super) result: Result<FavoriteSaveOutcome, FavoriteSaveError>,
}

type SaveTask = JoinHandle<(
    Box<dyn FavoriteStore>,
    Result<FavoriteSaveOutcome, FavoriteSaveError>,
)>;

enum SaveJob {
    Favorite {
        connection: String,
        model_id: String,
        desired: bool,
    },
    Engine(Box<EngineSaveJob>),
}

struct InFlightSave {
    job: SaveJob,
    task: SaveTask,
}

/// See the [module documentation](self).
pub(super) struct FavoriteSaveOwner {
    /// The writer while idle; `None` while a write is in flight or disabled.
    store: Option<Box<dyn FavoriteStore>>,
    /// Why persistence is disabled for the whole session, if it is.
    disabled_guidance: Option<String>,
    /// Authoritative membership, keyed by connections with a backing table.
    membership: HashMap<String, Vec<String>>,
    in_flight: Option<InFlightSave>,
    /// Validation facts for the engine save form, from the startup profile.
    engine_context: EngineSaveContext,
}

impl Default for FavoriteSaveOwner {
    fn default() -> Self {
        Self::from_setup(FavoriteSaveSetup::default())
    }
}

impl FavoriteSaveOwner {
    pub(super) fn from_setup(setup: FavoriteSaveSetup) -> Self {
        let engine_context = setup.engine_save_context([]);
        let FavoriteSaveSetup {
            writer,
            favorites_by_connection,
            ..
        } = setup;
        let store = writer.map(|writer| Box::new(writer) as Box<dyn FavoriteStore>);
        let mut owner = Self::from_parts(store, favorites_by_connection);
        owner.engine_context = engine_context;
        owner
    }

    /// Validation facts for a fresh [`AppState`], with every runtime engine
    /// name (explicit or implicit) reserved.
    pub(super) fn engine_save_context(
        &self,
        runtime_engine_names: impl IntoIterator<Item = String>,
    ) -> EngineSaveContext {
        let mut context = self.engine_context.clone();
        context.occupied_names.extend(runtime_engine_names);
        context
    }

    fn from_parts(
        store: Option<Box<dyn FavoriteStore>>,
        membership: HashMap<String, Vec<String>>,
    ) -> Self {
        let disabled_guidance = store
            .is_none()
            .then(|| NO_PROFILE_FILE_GUIDANCE.to_string());
        Self {
            store,
            disabled_guidance,
            membership,
            in_flight: None,
            engine_context: EngineSaveContext::default(),
        }
    }

    /// Whether a write has been started and not yet completed.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn is_pending(&self) -> bool {
        self.in_flight.is_some()
    }

    /// Confirmed favorite IDs of `connection` in configured order, newly
    /// saved IDs appended.
    pub(super) fn favorites_of(&self, connection: &str) -> &[String] {
        self.membership
            .get(connection)
            .map_or(&[], |favorites| favorites.as_slice())
    }

    /// Start persisting the desired membership of `model_id` in `connection`.
    ///
    /// Desired state, not a toggle. Must run inside a Tokio runtime.
    pub(super) fn request(
        &mut self,
        connection: &str,
        model_id: &str,
        desired: bool,
    ) -> FavoriteRequestOutcome {
        if self.in_flight.is_some() {
            return FavoriteRequestOutcome::IgnoredPending;
        }
        if let Some(guidance) = &self.disabled_guidance {
            return FavoriteRequestOutcome::Unavailable(guidance.clone());
        }
        if !self.membership.contains_key(connection) {
            return FavoriteRequestOutcome::Unavailable(format!(
                "favorites cannot be saved: provider connection '{connection}' has no \
                 [provider_connections.{connection}] table in the profile (environment-only \
                 or legacy engine); add one to the profile to enable favorites"
            ));
        }
        let Some(mut store) = self.store.take() else {
            return FavoriteRequestOutcome::Unavailable(SAVE_TASK_FAILED_GUIDANCE.to_string());
        };
        let task_connection = connection.to_string();
        let task_model_id = model_id.to_string();
        let task = tokio::task::spawn_blocking(move || {
            let result = store.set_favorite(&task_connection, &task_model_id, desired);
            (store, result)
        });
        self.in_flight = Some(InFlightSave {
            job: SaveJob::Favorite {
                connection: connection.to_string(),
                model_id: model_id.to_string(),
                desired,
            },
            task,
        });
        FavoriteRequestOutcome::Started
    }

    /// Start persisting one engine create or update. Shares the pipeline of
    /// [`Self::request`], so favorite and engine writes never overlap.
    pub(super) fn request_engine(&mut self, job: EngineSaveJob) -> FavoriteRequestOutcome {
        if self.in_flight.is_some() {
            return FavoriteRequestOutcome::IgnoredPending;
        }
        if let Some(guidance) = &self.disabled_guidance {
            return FavoriteRequestOutcome::Unavailable(guidance.clone());
        }
        let connection = &job.request.provider_connection;
        if !self.membership.contains_key(connection) {
            return FavoriteRequestOutcome::Unavailable(format!(
                "engines cannot be saved: provider connection '{connection}' has no \
                 [provider_connections.{connection}] table in the profile"
            ));
        }
        let Some(mut store) = self.store.take() else {
            return FavoriteRequestOutcome::Unavailable(SAVE_TASK_FAILED_GUIDANCE.to_string());
        };
        let task_request = job.request.clone();
        let task = tokio::task::spawn_blocking(move || {
            let result = store.save_engine(&task_request);
            (store, result)
        });
        self.in_flight = Some(InFlightSave {
            job: SaveJob::Engine(Box::new(job)),
            task,
        });
        FavoriteRequestOutcome::Started
    }

    /// Wait for the in-flight write, if any, and settle its state.
    ///
    /// Cancel-safe: dropping the future before it resolves leaves the write
    /// in flight; state changes happen only after the last await point.
    async fn wait_for_in_flight(&mut self) -> Option<SaveCompletion> {
        let in_flight = self.in_flight.as_mut()?;
        let joined = (&mut in_flight.task).await;
        let InFlightSave { job, .. } = self.in_flight.take()?;
        let result = match joined {
            Ok((store, result)) => {
                self.store = Some(store);
                result
            }
            Err(error) => {
                tracing::error!(%error, "profile save task failed");
                self.disabled_guidance = Some(SAVE_TASK_FAILED_GUIDANCE.to_string());
                Err(FavoriteSaveError::Unsupported {
                    reason: SAVE_TASK_FAILED_GUIDANCE.to_string(),
                })
            }
        };
        Some(match job {
            SaveJob::Favorite {
                connection,
                model_id,
                desired,
            } => {
                if result.is_ok() {
                    self.publish_membership(&connection, &model_id, desired);
                }
                SaveCompletion::Favorite(FavoriteSaveCompletion {
                    connection,
                    model_id,
                    desired,
                    result,
                })
            }
            SaveJob::Engine(job) => {
                SaveCompletion::Engine(Box::new(EngineSaveCompletion { job: *job, result }))
            }
        })
    }

    /// Resolve with the next save completion; never resolves while idle, so
    /// it suits a `select!` branch.
    pub(super) async fn next_completion(&mut self) -> SaveCompletion {
        match self.wait_for_in_flight().await {
            Some(completion) => completion,
            None => std::future::pending().await,
        }
    }

    /// Normal-shutdown hook: await a started write and return its completion.
    pub(super) async fn finish(&mut self) -> Option<SaveCompletion> {
        self.wait_for_in_flight().await
    }

    /// Make a confirmed change authoritative.
    fn publish_membership(&mut self, connection: &str, model_id: &str, desired: bool) {
        let favorites = self.membership.entry(connection.to_string()).or_default();
        if desired {
            if !favorites.iter().any(|favorite| favorite == model_id) {
                favorites.push(model_id.to_string());
            }
        } else {
            favorites.retain(|favorite| favorite != model_id);
        }
    }
}

/// Transcript text for a finished save.
///
/// `profile_name` selects the `ylva trust` command shown for workspace saves.
pub(super) fn completion_notice(completion: &FavoriteSaveCompletion, profile_name: &str) -> String {
    let identity = format!("{}/{}", completion.connection, completion.model_id);
    match &completion.result {
        Ok(outcome) => {
            let mut notice = match (outcome.changed, completion.desired) {
                (true, true) => format!("Marked {identity} as a favorite."),
                (true, false) => format!("Removed {identity} from the favorites."),
                (false, true) => format!("{identity} is already a favorite."),
                (false, false) => format!("{identity} is not a favorite."),
            };
            if outcome.publication_uncertain {
                notice.push_str(
                    " The file was confirmed after an error while publishing it, so the change \
                     may not survive a crash.",
                );
            }
            if outcome.changed && outcome.layer == FavoriteSourceLayer::Workspace {
                let profile_option = if profile_name == crate::config::DEFAULT_PROFILE_NAME {
                    String::new()
                } else {
                    format!("--profile {profile_name} ")
                };
                notice.push_str(&format!(
                    " It was written to the workspace profile {}: the next launch of Ylva \
                     requires `ylva {profile_option}trust` before it loads that configuration.",
                    display_path(&outcome.path)
                ));
            }
            notice
        }
        Err(error) => {
            let action = if completion.desired {
                "mark as a favorite"
            } else {
                "remove from the favorites"
            };
            let mut notice = format!("Could not {action} {identity}: {error}.");
            if matches!(
                error,
                FavoriteSaveError::SourceChanged { .. }
                    | FavoriteSaveError::PostPublishUncertain { .. }
            ) {
                notice.push_str(" Restart Ylva before saving favorites again.");
            }
            notice
        }
    }
}

/// Apply a finished save to the visible state and tell the user.
///
/// Runs regardless of popup or session visibility: a successful change is
/// pushed into the open picker and into a parked approval/clarify snapshot.
pub(super) fn apply_favorite_completion(
    state: &mut AppState,
    engine: &Arc<dyn crate::llm::engine::Engine>,
    completion: &FavoriteSaveCompletion,
) {
    match &completion.result {
        Ok(outcome) => {
            tracing::info!(
                connection = %completion.connection,
                model = %completion.model_id,
                favorite = completion.desired,
                changed = outcome.changed,
                publication_uncertain = outcome.publication_uncertain,
                "favorite saved"
            );
            engine.record_favorite_membership(
                &completion.connection,
                &completion.model_id,
                completion.desired,
            );
            state.apply_favorite_membership(
                &completion.connection,
                &completion.model_id,
                completion.desired,
            );
        }
        Err(error) => {
            tracing::warn!(
                connection = %completion.connection,
                model = %completion.model_id,
                favorite = completion.desired,
                %error,
                "favorite could not be saved"
            );
        }
    }
    let profile_name = state.display_metadata.profile_name.clone();
    state.push_notice(completion_notice(completion, &profile_name));
}

/// Notice for a request the owner did not start.
pub(super) fn rejected_request_notice(outcome: &FavoriteRequestOutcome) -> Option<String> {
    match outcome {
        FavoriteRequestOutcome::Started => None,
        FavoriteRequestOutcome::IgnoredPending => {
            Some("A favorite is still being saved; the repeated request was ignored.".to_string())
        }
        FavoriteRequestOutcome::Unavailable(guidance) => Some(guidance.clone()),
    }
}

/// Route [`Effect::SetFavoriteModel`] and [`Effect::SaveEngine`] effects to
/// the owner, reporting requests it did not start (pending write,
/// unavailable persistence) as transcript notices or, for an engine save, on
/// the save page. Other effects are ignored.
pub(super) fn start_favorite_saves(
    state: &mut AppState,
    owner: &mut FavoriteSaveOwner,
    effects: Vec<Effect>,
) {
    for effect in effects {
        match effect {
            Effect::SetFavoriteModel {
                provider_connection,
                model_id,
                favorite,
            } => {
                let outcome = owner.request(&provider_connection, &model_id, favorite);
                if let Some(notice) = rejected_request_notice(&outcome) {
                    tracing::info!(
                        connection = %provider_connection,
                        model = %model_id,
                        ?outcome,
                        "favorite save request not started"
                    );
                    state.push_notice(notice);
                }
            }
            Effect::SaveEngine(job) => {
                let engine_name = job.request.mode.name().to_string();
                match owner.request_engine(*job) {
                    FavoriteRequestOutcome::Started => {}
                    FavoriteRequestOutcome::IgnoredPending => state.fail_engine_save(
                        "another profile save is still running; try again in a moment".to_string(),
                    ),
                    FavoriteRequestOutcome::Unavailable(guidance) => {
                        tracing::info!(engine = %engine_name, "engine save request not started");
                        state.fail_engine_save(guidance);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Name the next launch's trust requirement for a workspace destination.
fn workspace_trust_guidance(path: &std::path::Path, profile_name: &str) -> String {
    let profile_option = if profile_name == crate::config::DEFAULT_PROFILE_NAME {
        String::new()
    } else {
        format!("--profile {profile_name} ")
    };
    format!(
        " It was written to the workspace profile {}: the next launch of Ylva requires \
         `ylva {profile_option}trust` before it loads that configuration.",
        display_path(path)
    )
}

/// Transcript text for a finished engine save that was not published.
pub(super) fn engine_completion_notice(
    completion: &EngineSaveCompletion,
    profile_name: &str,
) -> String {
    let name = completion.job.request.mode.name();
    match &completion.result {
        Ok(outcome) => {
            let mut notice = if outcome.changed {
                format!("Saved engine '{name}' to {}.", display_path(&outcome.path))
            } else {
                format!("Engine '{name}' already has these settings; nothing was written.")
            };
            if outcome.publication_uncertain {
                notice.push_str(
                    " The file was confirmed after an error while publishing it, so the change \
                     may not survive a crash.",
                );
            }
            if outcome.changed && outcome.layer == FavoriteSourceLayer::Workspace {
                notice.push_str(&workspace_trust_guidance(&outcome.path, profile_name));
            }
            notice
        }
        Err(error) => {
            let mut notice = format!("Could not save engine '{name}': {error}.");
            if matches!(
                error,
                FavoriteSaveError::SourceChanged { .. }
                    | FavoriteSaveError::PostPublishUncertain { .. }
            ) {
                notice.push_str(" Restart Ylva before saving again.");
            }
            notice
        }
    }
}

/// Publish a persisted engine to future roots and the live root.
fn publish_saved_engine(
    owner: &FavoriteSaveOwner,
    driver: Option<&TitleDriver>,
    live_root: &Arc<dyn Engine>,
    name: &str,
    config: &EngineConfig,
    update: bool,
) -> anyhow::Result<()> {
    let driver = driver.ok_or_else(|| anyhow::anyhow!("this session has no engine factory"))?;
    let mut provider_connections = driver.profile.provider_connections.clone();
    for (connection_name, connection) in &mut provider_connections {
        if owner.membership.contains_key(connection_name) {
            connection.favorite_models = owner.favorites_of(connection_name).to_vec();
        }
    }
    let engine = driver
        .factory
        .register_engine(name, config, &provider_connections, update)?;
    let publication = if update {
        NamedEnginePublication::Replace
    } else {
        NamedEnginePublication::Add
    };
    live_root.publish_named_engine(name, engine, publication)
}

/// What the event loop still does after [`apply_engine_completion`].
#[derive(Debug, Default)]
pub(super) struct EngineSaveFollowUp {
    /// The picker was closed, so the input draft that opened it is cleared.
    pub(super) picker_closed: bool,
    /// Selection to activate: Save & apply, or re-applying an updated active
    /// engine to the running session.
    pub(super) activation: Option<ModelSelection>,
    /// Session state from before the engine was replaced, when the active
    /// engine was updated; model history is recorded only if it changed.
    pub(super) previous_state: Option<ModelSnapshot>,
}

/// Apply a finished engine save: report it, publish the engine and, for
/// Save & apply while the submitting save page is still open, request its
/// activation.
pub(super) fn apply_engine_completion(
    state: &mut AppState,
    completion: &EngineSaveCompletion,
    owner: &FavoriteSaveOwner,
    driver: Option<&TitleDriver>,
    engine: &Arc<dyn Engine>,
) -> EngineSaveFollowUp {
    let profile_name = state.display_metadata.profile_name.clone();
    let mut notice = engine_completion_notice(completion, &profile_name);
    let job = &completion.job;
    let name = job.request.mode.name().to_string();
    let Ok(_) = &completion.result else {
        tracing::warn!(engine = %name, "engine could not be saved");
        state.fail_engine_save(notice.clone());
        state.push_notice(notice);
        return EngineSaveFollowUp::default();
    };
    tracing::info!(engine = %name, "engine saved");
    let update = matches!(job.request.mode, EngineSaveMode::Update { .. });
    let origin_name = match &job.request.mode {
        EngineSaveMode::Update { name } => Some(name.as_str()),
        EngineSaveMode::Create { copy_from, .. } => copy_from.as_deref(),
    };
    let origin = origin_name
        .and_then(|origin_name| state.engine_save_context.explicit_engines.get(origin_name))
        .cloned();
    let config = job.request.resulting_config(origin.as_ref());
    state.record_saved_engine(&name, config.clone());
    let form_is_open = state.engine_save_is_pending();
    let replaces_active = update && engine.active_engine_name().as_deref() == Some(name.as_str());
    let previous_state = replaces_active.then(|| ModelSnapshot::of(engine.as_ref()));
    match publish_saved_engine(owner, driver, engine, &name, &config, update) {
        Ok(()) => {
            if job.activate && !form_is_open {
                notice.push_str(" Save & apply was skipped because the save page was left.");
            }
        }
        Err(error) => {
            tracing::warn!(engine = %name, %error, "saved engine could not be published");
            notice.push_str(&format!(
                " It could not be made available in this session ({error}); restart Ylva to use it."
            ));
        }
    }
    let published = !notice.contains("could not be made available");
    if form_is_open {
        state.close_model_picker_after_save();
    }
    state.push_notice(notice);
    let mut activation = None;
    let activate = (job.activate && form_is_open) || replaces_active;
    if activate {
        if published {
            activation = Some(ModelSelection::NamedEngine {
                engine_name: name,
                option: job.activation_option.clone(),
                reasoning_effort: ReasoningEffortChange::Unchanged,
            });
        } else if job.activate {
            state.push_warning_notice(
                "Saved, but could not activate the engine in this session.".to_string(),
            );
        }
    }
    EngineSaveFollowUp {
        picker_closed: form_is_open,
        activation,
        previous_state: previous_state.filter(|_| published),
    }
}
