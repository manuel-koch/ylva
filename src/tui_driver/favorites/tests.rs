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

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{mpsc as std_mpsc, Arc, Mutex};

use super::*;
use crate::tui_driver::effects::{execute_effects, split_slash_effects, PendingRunIds};
use crate::tui_driver::input::MouseDrag;
use crate::tui_driver::NoopClipboard;
use crate::tui_reducer_state::{ChatEntry, SlashOverlay};

fn empty_engine() -> Arc<dyn crate::llm::engine::Engine> {
    use crate::llm::{EngineConfig, EngineImpl, MockProvider};
    use std::time::Duration;

    Arc::new(EngineImpl::new(
        Arc::new(MockProvider::new(Vec::new(), Duration::ZERO)),
        EngineConfig {
            provider_connection: "stub".to_string(),
            model: "model".to_string(),
            temperature: None,
            context_length: None,
            api_base_url: None,
            api_key: None,
            reasoning: None,
            compact_target_ratio: 0.5,
            soft_compaction_threshold: 0.7,
            hard_compaction_threshold: 0.9,
        },
    ))
}

type ScriptedResults = Arc<Mutex<VecDeque<Result<FavoriteSaveOutcome, FavoriteSaveError>>>>;

fn saved_outcome(layer: FavoriteSourceLayer, changed: bool) -> FavoriteSaveOutcome {
    FavoriteSaveOutcome {
        layer,
        path: PathBuf::from("/profiles/default/config.toml"),
        changed,
        publication_uncertain: false,
    }
}

/// Store double: answers from a script, optionally holding every write open
/// until the test releases it.
struct ScriptedStore {
    results: ScriptedResults,
    calls: Arc<Mutex<Vec<(String, String, bool)>>>,
    started: Option<std_mpsc::Sender<()>>,
    release: Option<std_mpsc::Receiver<()>>,
}

impl FavoriteStore for ScriptedStore {
    fn set_favorite(
        &mut self,
        connection: &str,
        model_id: &str,
        favorite: bool,
    ) -> Result<FavoriteSaveOutcome, FavoriteSaveError> {
        self.calls.lock().expect("calls").push((
            connection.to_string(),
            model_id.to_string(),
            favorite,
        ));
        if let Some(started) = &self.started {
            started.send(()).expect("test observes the start");
        }
        if let Some(release) = &self.release {
            release.recv().expect("test releases the write");
        }
        self.results
            .lock()
            .expect("results")
            .pop_front()
            .expect("scripted result")
    }
}

struct Fixture {
    owner: FavoriteSaveOwner,
    results: ScriptedResults,
    calls: Arc<Mutex<Vec<(String, String, bool)>>>,
    started: Option<std_mpsc::Receiver<()>>,
    release: Option<std_mpsc::Sender<()>>,
}

/// Owner over a scripted store knowing connection `stub` with favorites `["kept"]`.
fn fixture(script: Vec<Result<FavoriteSaveOutcome, FavoriteSaveError>>, gated: bool) -> Fixture {
    let results: ScriptedResults = Arc::new(Mutex::new(script.into()));
    let calls = Arc::default();
    let (started_tx, started_rx) = std_mpsc::channel();
    let (release_tx, release_rx) = std_mpsc::channel();
    let store = ScriptedStore {
        results: Arc::clone(&results),
        calls: Arc::clone(&calls),
        started: gated.then_some(started_tx),
        release: gated.then_some(release_rx),
    };
    let owner = FavoriteSaveOwner::from_parts(
        Some(Box::new(store)),
        HashMap::from([("stub".to_string(), vec!["kept".to_string()])]),
    );
    Fixture {
        owner,
        results,
        calls,
        started: gated.then_some(started_rx),
        release: gated.then_some(release_tx),
    }
}

fn notices(state: &AppState) -> Vec<String> {
    state
        .chat_entries
        .iter()
        .filter_map(|entry| match entry {
            ChatEntry::Notice { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn repeated_requests_are_ignored_while_a_write_is_pending() {
    let mut fixture = fixture(
        vec![Ok(saved_outcome(FavoriteSourceLayer::Home, true))],
        true,
    );

    assert_eq!(
        fixture.owner.request("stub", "model-a", true),
        FavoriteRequestOutcome::Started
    );
    fixture
        .started
        .as_ref()
        .expect("gated")
        .recv()
        .expect("write started");
    assert!(fixture.owner.is_pending());
    assert_eq!(
        fixture.owner.request("stub", "model-b", true),
        FavoriteRequestOutcome::IgnoredPending
    );
    assert_eq!(
        fixture.owner.request("stub", "model-a", false),
        FavoriteRequestOutcome::IgnoredPending
    );

    fixture
        .release
        .as_ref()
        .expect("gated")
        .send(())
        .expect("release");
    let completion = fixture.owner.next_completion().await;

    assert!(completion.result.is_ok());
    assert!(!fixture.owner.is_pending());
    assert_eq!(
        fixture.calls.lock().expect("calls").as_slice(),
        [("stub".to_string(), "model-a".to_string(), true)],
        "ignored requests never reach the writer"
    );
}

#[tokio::test]
async fn membership_is_published_only_after_confirmed_persistence() {
    let mut fixture = fixture(
        vec![Ok(saved_outcome(FavoriteSourceLayer::Home, true))],
        true,
    );

    fixture.owner.request("stub", "model-a", true);
    fixture
        .started
        .as_ref()
        .expect("gated")
        .recv()
        .expect("write started");
    assert_eq!(fixture.owner.favorites_of("stub"), ["kept"]);

    fixture
        .release
        .as_ref()
        .expect("gated")
        .send(())
        .expect("release");
    let completion = fixture.owner.next_completion().await;

    assert!(completion.result.is_ok());
    assert_eq!(fixture.owner.favorites_of("stub"), ["kept", "model-a"]);
    let engine = empty_engine();
    apply_favorite_completion(&mut AppState::new(), &engine, &completion);
    assert_eq!(
        engine.favorite_models(),
        [("stub".to_string(), vec!["model-a".to_string()])]
    );

    // Removal follows the same rule and drops every exact duplicate.
    fixture
        .results
        .lock()
        .expect("results")
        .push_back(Ok(saved_outcome(FavoriteSourceLayer::Home, true)));
    fixture.owner.request("stub", "kept", false);
    fixture
        .release
        .as_ref()
        .expect("gated")
        .send(())
        .expect("release");
    fixture.owner.next_completion().await;
    assert_eq!(fixture.owner.favorites_of("stub"), ["model-a"]);
}

#[tokio::test]
async fn failed_save_keeps_membership_and_a_retry_reuses_the_writer() {
    let mut fixture = fixture(
        vec![
            Err(FavoriteSaveError::Retryable {
                path: PathBuf::from("/profiles/default/config.toml"),
                detail: "disk busy".to_string(),
            }),
            Ok(saved_outcome(FavoriteSourceLayer::Home, true)),
        ],
        false,
    );
    let mut state = AppState::new();
    let engine = empty_engine();

    assert_eq!(
        fixture.owner.request("stub", "model-a", true),
        FavoriteRequestOutcome::Started
    );
    let failure = fixture.owner.next_completion().await;
    apply_favorite_completion(&mut state, &engine, &failure);

    assert!(failure
        .result
        .as_ref()
        .is_err_and(|error| error.is_retryable()));
    assert_eq!(fixture.owner.favorites_of("stub"), ["kept"]);
    assert!(engine.favorite_models().is_empty());
    let failure_notice = notices(&state).pop().expect("failure notice");
    assert!(failure_notice.contains("disk busy"), "{failure_notice}");
    assert!(failure_notice.contains("try again"), "{failure_notice}");
    assert!(!failure_notice.contains("Restart Ylva"), "{failure_notice}");

    assert_eq!(
        fixture.owner.request("stub", "model-a", true),
        FavoriteRequestOutcome::Started
    );
    let success = fixture.owner.next_completion().await;
    apply_favorite_completion(&mut state, &engine, &success);

    assert!(success.result.is_ok());
    assert_eq!(fixture.owner.favorites_of("stub"), ["kept", "model-a"]);
    assert_eq!(fixture.calls.lock().expect("calls").len(), 2);
    assert!(notices(&state)
        .pop()
        .expect("success notice")
        .contains("Marked stub/model-a as a favorite"));
}

#[tokio::test]
async fn source_change_reports_restart_guidance_and_keeps_membership() {
    let mut fixture = fixture(
        vec![Err(FavoriteSaveError::SourceChanged {
            path: PathBuf::from("/profiles/default/config.toml"),
            detail: "contents differ".to_string(),
        })],
        false,
    );
    let mut state = AppState::new();

    fixture.owner.request("stub", "model-a", true);
    let completion = fixture.owner.next_completion().await;
    apply_favorite_completion(&mut state, &empty_engine(), &completion);

    assert_eq!(fixture.owner.favorites_of("stub"), ["kept"]);
    let notice = notices(&state).pop().expect("notice");
    assert!(
        notice.contains("Restart Ylva before saving favorites again"),
        "{notice}"
    );
}

#[tokio::test]
async fn unavailable_capability_explains_why() {
    let mut disabled = FavoriteSaveOwner::default();
    let FavoriteRequestOutcome::Unavailable(guidance) = disabled.request("stub", "m", true) else {
        panic!("a disabled owner reports unavailability");
    };
    assert!(guidance.contains("built-in defaults"), "{guidance}");
    assert!(!disabled.is_pending());

    let mut fixture = fixture(vec![], false);
    let FavoriteRequestOutcome::Unavailable(guidance) =
        fixture.owner.request("env-only", "m", true)
    else {
        panic!("a connection without a table is unavailable");
    };
    assert!(guidance.contains("env-only"), "{guidance}");
    assert!(fixture.calls.lock().expect("calls").is_empty());

    let mut state = AppState::new();
    start_favorite_saves(
        &mut state,
        &mut fixture.owner,
        vec![Effect::SetFavoriteModel {
            provider_connection: "env-only".to_string(),
            model_id: "m".to_string(),
            favorite: true,
        }],
    );
    assert_eq!(notices(&state), [guidance]);
}

#[tokio::test]
async fn pending_repeat_from_the_effect_path_adds_a_brief_notice() {
    let mut fixture = fixture(
        vec![Ok(saved_outcome(FavoriteSourceLayer::Home, true))],
        true,
    );
    let mut state = AppState::new();
    let effect = |model: &str| Effect::SetFavoriteModel {
        provider_connection: "stub".to_string(),
        model_id: model.to_string(),
        favorite: true,
    };

    start_favorite_saves(
        &mut state,
        &mut fixture.owner,
        vec![effect("model-a"), effect("model-b")],
    );
    fixture
        .release
        .as_ref()
        .expect("gated")
        .send(())
        .expect("release");
    fixture.owner.next_completion().await;

    assert_eq!(notices(&state).len(), 1);
    assert!(notices(&state)[0].contains("still being saved"));
}

#[tokio::test]
async fn late_completion_applies_after_the_popup_closed_and_after_clear() {
    let mut fixture = fixture(
        vec![Ok(saved_outcome(FavoriteSourceLayer::Home, true))],
        true,
    );
    let mut state = AppState::new();
    state.slash_overlay = SlashOverlay::LoadingModels;

    fixture.owner.request("stub", "model-a", true);
    fixture
        .started
        .as_ref()
        .expect("gated")
        .recv()
        .expect("write started");
    // The popup closes and /clear replaces the conversation while the write is held open.
    state.slash_overlay = SlashOverlay::Hidden;
    state.push_notice("old conversation".to_string());
    state.clear_conversation();
    fixture
        .release
        .as_ref()
        .expect("gated")
        .send(())
        .expect("release");
    let completion = fixture.owner.next_completion().await;
    apply_favorite_completion(&mut state, &empty_engine(), &completion);

    assert_eq!(fixture.owner.favorites_of("stub"), ["kept", "model-a"]);
    let notices = notices(&state);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].contains("Marked stub/model-a"));
}

#[tokio::test]
async fn finish_awaits_a_started_write_and_surfaces_its_outcome() {
    let mut fixture = fixture(
        vec![Ok(saved_outcome(FavoriteSourceLayer::Home, true))],
        true,
    );
    assert!(
        fixture.owner.finish().await.is_none(),
        "idle owner finishes at once"
    );

    fixture.owner.request("stub", "model-a", true);
    fixture
        .started
        .as_ref()
        .expect("gated")
        .recv()
        .expect("write started");

    let mut finishing = Box::pin(fixture.owner.finish());
    assert!(
        futures::poll!(finishing.as_mut()).is_pending(),
        "shutdown waits while the write is held open"
    );
    fixture
        .release
        .as_ref()
        .expect("gated")
        .send(())
        .expect("release");
    let completion = finishing.await.expect("the started write's completion");
    assert!(completion.result.is_ok());
    assert_eq!(completion.model_id, "model-a");
    assert_eq!(fixture.owner.favorites_of("stub"), ["kept", "model-a"]);
    assert!(!fixture.owner.is_pending());
}

#[tokio::test]
async fn next_completion_stays_pending_while_idle() {
    let mut fixture = fixture(vec![], false);
    let mut next = Box::pin(fixture.owner.next_completion());
    assert!(futures::poll!(next.as_mut()).is_pending());
}

#[test]
fn workspace_saves_warn_that_the_next_launch_needs_trust() {
    let completion = |layer| FavoriteSaveCompletion {
        connection: "stub".to_string(),
        model_id: "m".to_string(),
        desired: true,
        result: Ok(saved_outcome(layer, true)),
    };

    let workspace = completion_notice(&completion(FavoriteSourceLayer::Workspace), "default");
    assert!(workspace.contains("`ylva trust`"), "{workspace}");
    let named = completion_notice(&completion(FavoriteSourceLayer::Workspace), "work");
    assert!(named.contains("`ylva --profile work trust`"), "{named}");
    let home = completion_notice(&completion(FavoriteSourceLayer::Home), "default");
    assert!(!home.contains("trust"), "{home}");
}

#[test]
fn setup_covers_only_connections_with_a_backing_table() {
    let profile: ProfileConfig = toml::from_str(
        r#"
        [provider_connections.local]
        provider = "ollama"
        favorite_models = ["a", "b"]
        "#,
    )
    .expect("profile parses");

    let setup = FavoriteSaveSetup::from_profile(None, &profile);

    assert_eq!(
        setup.favorites_by_connection,
        HashMap::from([("local".to_string(), vec!["a".to_string(), "b".to_string()])])
    );
    assert!(setup.writer.is_none());
}

#[tokio::test]
async fn real_writer_persists_through_the_owner() {
    let directory = tempfile::Builder::new()
        .prefix("ylva-favorites-")
        .tempdir()
        .expect("tempdir");
    let profile_path = directory.path().join("config.toml");
    let raw = "# keep me\n[provider_connections.local]\nprovider = \"ollama\"\n";
    std::fs::write(&profile_path, raw).expect("write profile");
    let profile: ProfileConfig = toml::from_str(raw).expect("profile parses");
    let writer = FavoriteModelsWriter::from_loaded_source(
        profile_path.clone(),
        raw.to_string(),
        FavoriteSourceLayer::Home,
    );
    let mut owner =
        FavoriteSaveOwner::from_setup(FavoriteSaveSetup::from_profile(Some(writer), &profile));

    assert_eq!(
        owner.request("local", "llama3", true),
        FavoriteRequestOutcome::Started
    );
    let completion = owner.finish().await.expect("completion");

    assert!(completion.result.is_ok(), "{:?}", completion.result);
    let saved = std::fs::read_to_string(&profile_path).expect("read profile");
    assert!(saved.contains("# keep me"), "{saved}");
    assert!(saved.contains("favorite_models = [\"llama3\"]"), "{saved}");
    assert_eq!(owner.favorites_of("local"), ["llama3"]);

    // An outside edit is not rebased: the next save reports restart guidance.
    std::fs::write(&profile_path, format!("{saved}# edited\n")).expect("edit");
    owner.request("local", "other", true);
    let completion = owner.finish().await.expect("completion");
    assert!(matches!(
        completion.result,
        Err(FavoriteSaveError::SourceChanged { .. })
    ));
    assert_eq!(owner.favorites_of("local"), ["llama3"]);
}

#[tokio::test]
async fn favorite_effect_is_routed_to_the_owner_not_the_agent() {
    let effect = Effect::SetFavoriteModel {
        provider_connection: "stub".to_string(),
        model_id: "m".to_string(),
        favorite: true,
    };
    let (slash_effects, other_effects) = split_slash_effects(vec![Effect::StopAgent, effect]);
    assert_eq!(slash_effects.len(), 1);
    assert!(matches!(slash_effects[0], Effect::SetFavoriteModel { .. }));
    assert_eq!(other_effects.len(), 1);

    // Even if one slips into the generic executor, it sends nothing.
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel(4);
    let mut state = AppState::new();
    let mut textarea = ratatui_textarea::TextArea::default();
    let outcome = execute_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        slash_effects,
        &mut MouseDrag::default(),
        &mut NoopClipboard,
        &mut PendingRunIds::default(),
    )
    .await;
    assert_eq!(outcome, crate::tui_driver::ActionOutcome::Continue);
    assert!(cmd_rx.try_recv().is_err());
    assert!(state.chat_entries.is_empty());
}
