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

use crate::config::{
    FavoriteModelsWriter, FavoriteSaveError, FavoriteSaveOutcome, FavoriteSourceLayer,
    ProfileConfig,
};
use crate::home_relative_path::display_path;
use crate::tui_reducer_state::{AppState, Effect};

#[cfg(test)]
mod tests;

/// Guidance shown when the session has no profile file to save favorites to.
const NO_PROFILE_FILE_GUIDANCE: &str = "favorites cannot be saved: this session is not backed by \
a profile config file (built-in defaults); run `ylva connect <provider>` to create a profile";

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
        Self {
            writer,
            favorites_by_connection,
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

/// Finished save, delivered by [`FavoriteSaveOwner::next_completion`].
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

struct InFlightSave {
    connection: String,
    model_id: String,
    desired: bool,
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
}

impl Default for FavoriteSaveOwner {
    fn default() -> Self {
        Self::from_setup(FavoriteSaveSetup::default())
    }
}

impl FavoriteSaveOwner {
    pub(super) fn from_setup(setup: FavoriteSaveSetup) -> Self {
        let FavoriteSaveSetup {
            writer,
            favorites_by_connection,
        } = setup;
        let store = writer.map(|writer| Box::new(writer) as Box<dyn FavoriteStore>);
        Self::from_parts(store, favorites_by_connection)
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
        }
    }

    /// Whether a write has been started and not yet completed.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn is_pending(&self) -> bool {
        self.in_flight.is_some()
    }

    /// Confirmed favorite IDs of `connection` in configured order, newly
    /// saved IDs appended.
    #[cfg_attr(not(test), allow(dead_code))]
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
            connection: connection.to_string(),
            model_id: model_id.to_string(),
            desired,
            task,
        });
        FavoriteRequestOutcome::Started
    }

    /// Wait for the in-flight write, if any, and settle its state.
    ///
    /// Cancel-safe: dropping the future before it resolves leaves the write
    /// in flight; state changes happen only after the last await point.
    async fn wait_for_in_flight(&mut self) -> Option<FavoriteSaveCompletion> {
        let in_flight = self.in_flight.as_mut()?;
        let joined = (&mut in_flight.task).await;
        let InFlightSave {
            connection,
            model_id,
            desired,
            ..
        } = self.in_flight.take()?;
        let result = match joined {
            Ok((store, result)) => {
                self.store = Some(store);
                result
            }
            Err(error) => {
                tracing::error!(%error, "favorites save task failed");
                self.disabled_guidance = Some(SAVE_TASK_FAILED_GUIDANCE.to_string());
                Err(FavoriteSaveError::Unsupported {
                    reason: SAVE_TASK_FAILED_GUIDANCE.to_string(),
                })
            }
        };
        if result.is_ok() {
            self.publish_membership(&connection, &model_id, desired);
        }
        Some(FavoriteSaveCompletion {
            connection,
            model_id,
            desired,
            result,
        })
    }

    /// Resolve with the next save completion; never resolves while idle, so
    /// it suits a `select!` branch.
    pub(super) async fn next_completion(&mut self) -> FavoriteSaveCompletion {
        match self.wait_for_in_flight().await {
            Some(completion) => completion,
            None => std::future::pending().await,
        }
    }

    /// Normal-shutdown hook: await a started write and return its completion.
    pub(super) async fn finish(&mut self) -> Option<FavoriteSaveCompletion> {
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

/// Route [`Effect::SetFavoriteModel`] effects to the owner, reporting requests
/// it did not start (pending write, unavailable persistence) as transcript
/// notices. Other effects are ignored.
pub(super) fn start_favorite_saves(
    state: &mut AppState,
    owner: &mut FavoriteSaveOwner,
    effects: Vec<Effect>,
) {
    for effect in effects {
        let Effect::SetFavoriteModel {
            provider_connection,
            model_id,
            favorite,
        } = effect
        else {
            continue;
        };
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
}
