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

use super::super::models::ModelListResult;
use super::*;

#[tokio::test(start_paused = true)]
async fn cancelling_a_model_catalog_disables_waiting_until_a_new_request_starts() {
    let engine: Arc<dyn Engine> = StubEngine::with_models(&["small"], "small");
    let mut pending_model_list = None;
    start_model_list(
        &mut pending_model_list,
        Some("old filter".to_string()),
        &engine,
    );
    abort_pending_model_list(&mut pending_model_list);
    assert!(pending_model_list.is_none());
    assert!(
        tokio::time::timeout(
            Duration::from_secs(1),
            wait_for_model_list(&mut pending_model_list)
        )
        .await
        .is_err(),
        "the select branch must remain disabled without a catalog request"
    );

    start_model_list(
        &mut pending_model_list,
        Some("new filter".to_string()),
        &engine,
    );
    let (filter, result) = wait_for_model_list(&mut pending_model_list)
        .await
        .expect("the replacement catalog request completes");
    assert_eq!(filter.as_deref(), Some("new filter"));
    assert!(
        matches!(result, ModelListResult::Models(models) if models.len() == 1 && models[0].model.id == "small")
    );
    abort_pending_model_list(&mut pending_model_list);
    assert!(pending_model_list.is_none());
}

#[test]
fn compaction_switch_notices_explain_engine_selection_effects() {
    use crate::llm::engine::{CompactionEngineDisplayInfo, EngineDisplayInfo};

    let target = crate::llm::engine::ModelSelectionTarget {
        engine_name: Some("engine-b".to_string()),
        provider_connection: "mock".to_string(),
        model: "model-b".to_string(),
        context_length: 65_536,
    };
    let mut display = EngineDisplayInfo {
        engine_name: "engine-a".to_string(),
        provider_connection: "mock".to_string(),
        model: "model-a".to_string(),
        reasoning_effort: None,
        compaction_engine: Some(CompactionEngineDisplayInfo {
            engine_name: "engine-a".to_string(),
            provider_connection: "mock".to_string(),
            model: "model-a".to_string(),
            context_length: 131_072,
            follows_conversation: true,
            shares_conversation: true,
        }),
        inherits_parent_runtime: false,
    };
    assert_eq!(
        compaction_switch_notice(&display, &target).as_deref(),
        Some("Compaction now uses selected engine 'engine-b'.")
    );

    let compaction = display
        .compaction_engine
        .as_mut()
        .expect("summarizer details");
    compaction.follows_conversation = false;
    compaction.engine_name = "engine-a".to_string();
    assert_eq!(
        compaction_switch_notice(&display, &target).as_deref(),
        Some("Compaction remains on 'engine-a'.")
    );

    let shared_target = crate::llm::engine::ModelSelectionTarget {
        engine_name: Some("engine-a".to_string()),
        provider_connection: "mock".to_string(),
        model: "model-a".to_string(),
        context_length: 131_072,
    };
    assert_eq!(
        compaction_switch_notice(&display, &shared_target).as_deref(),
        Some("Compaction uses 'engine-a', shared with the conversation.")
    );
}

#[tokio::test]
async fn interrupted_model_list_discovery_resumes_after_the_pause() {
    // A result completing while a pause hid the overlay is discarded, so
    // once the pause restores `LoadingModels` the request must be made
    // again for the picker to ever populate.
    let (mut state, _, _cmd_tx, _cmd_rx) = setup();
    let engine: Arc<dyn Engine> = StubEngine::with_models(&["small", "large"], "large");
    let mut pending_model_list: Option<PendingModelList> = None;

    state.slash_overlay = SlashOverlay::LoadingModels;
    resume_interrupted_model_list(&state, &mut pending_model_list, &engine, "/models deep");
    assert!(
        pending_model_list.is_some(),
        "the discarded catalog request must be restarted"
    );

    let (filter, result) = wait_for_model_list(&mut pending_model_list)
        .await
        .expect("the restarted request must produce a result");
    pending_model_list = None;
    apply_model_list_result(&mut state, filter, result, &engine);
    let SlashOverlay::ModelPicker {
        choices, filter, ..
    } = &state.slash_overlay
    else {
        panic!("expected the models picker, got {:?}", state.slash_overlay);
    };
    assert_eq!(filter, "deep");
    assert_eq!(choices.len(), 2);

    // While the original request is still in flight, its result will be
    // applied on arrival — the helper must not start a second one.
    state.slash_overlay = SlashOverlay::LoadingModels;
    start_model_list(&mut pending_model_list, None, &engine);
    resume_interrupted_model_list(&state, &mut pending_model_list, &engine, "/models deep");
    let (filter, _) = wait_for_model_list(&mut pending_model_list)
        .await
        .expect("the original in-flight request completes");
    assert_eq!(filter, None, "an in-flight request must not be replaced");
    abort_pending_model_list(&mut pending_model_list);
}

#[test]
fn models_command_passes_a_typed_filter_instead_of_submitting_chat() {
    let (mut state, _textarea, _cmd_tx, _cmd_rx) = setup();
    let mut textarea = TextArea::default();

    let effects = submit(&mut state, &mut textarea, "/models deepseek");

    assert_eq!(
        effects,
        vec![
            Effect::SetInputText("/models deepseek".to_string()),
            Effect::ListModels {
                filter: Some("deepseek".to_string())
            }
        ]
    );
    assert!(
        !effects.contains(&Effect::SubmitPrompt { steer: false }),
        "a model filter is a slash-command argument, not a chat prompt"
    );
}

#[tokio::test]
async fn models_lists_the_candidates_and_preselects_the_active_model() {
    let (mut state, _textarea, cmd_tx, _cmd_rx) = setup();
    let engine: Arc<dyn Engine> = StubEngine::with_models(&["small", "large"], "large");

    execute_slash_effects(
        &mut state,
        vec![Effect::ListModels { filter: None }],
        &engine,
        &cmd_tx,
    )
    .await;

    let SlashOverlay::ModelPicker {
        choices, selected, ..
    } = &state.slash_overlay
    else {
        panic!("expected a model picker, got {:?}", state.slash_overlay);
    };
    assert_eq!(
        choices
            .iter()
            .map(|choice| choice.selection.model.id.as_str())
            .collect::<Vec<_>>(),
        vec!["small", "large"]
    );
    assert_eq!(
        choices
            .iter()
            .map(|choice| choice.label.as_str())
            .collect::<Vec<_>>(),
        vec![
            "stub/small  reasoning: unavailable",
            "stub/large  reasoning: unavailable  — active"
        ]
    );
    assert_eq!(*selected, 1);
}

#[tokio::test]
async fn models_lists_an_engine_shortcut_with_its_catalog_reasoning_efforts() {
    // Reproduces a profile's `[engines.<name>]` table naming a model the
    // catalog *does* advertise reasoning efforts for — the shortcut must
    // show those efforts (and the configured one), not "unavailable".
    let (mut state, _textarea, cmd_tx, _cmd_rx) = setup();
    let engine: Arc<dyn Engine> = StubEngine::with_named_engine(
        &["small", "thinking"],
        "small",
        "openrouter",
        "thinking",
        &["low", "high", "max"],
        Some("max"),
        Some(32_000),
        Some(999_999),
    );

    execute_slash_effects(
        &mut state,
        vec![Effect::ListModels { filter: None }],
        &engine,
        &cmd_tx,
    )
    .await;

    let SlashOverlay::ModelPicker { choices, .. } = &state.slash_overlay else {
        panic!("expected a model picker, got {:?}", state.slash_overlay);
    };
    let shortcut = choices
        .iter()
        .find(|choice| choice.engine_name.is_some())
        .expect("the named engine must appear as a shortcut");
    assert_eq!(
        shortcut.selection.model.reasoning_efforts,
        Some(vec![
            "low".to_string(),
            "high".to_string(),
            "max".to_string()
        ]),
        "the shortcut's model must carry the catalog's advertised reasoning efforts"
    );
    assert_eq!(
        shortcut.selection.model.context_length,
        Some(32_000),
        "the shortcut must show the engine's own configured context_length, \
         not the provider's advertised one (999_999)"
    );
    assert_eq!(
        shortcut.label, "openrouter → stub/thinking  context: 31kb  reasoning: max ⚙",
        "the shortcut must show its configured reasoning effort instead of 'unavailable'"
    );
}

#[tokio::test]
async fn models_lists_an_engine_shortcut_with_its_catalog_reasoning_default() {
    // The catalog's provider-advertised default effort must reach the
    // shortcut row, so its default slot reads `default (medium)` instead
    // of a bare `default`.
    let (mut state, _textarea, cmd_tx, _cmd_rx) = setup();
    let mut stub = StubEngine::with_named_engine(
        &["small", "thinking"],
        "small",
        "openrouter",
        "thinking",
        &["low", "high"],
        None,
        Some(32_000),
        None,
    );
    for model in Arc::get_mut(&mut stub)
        .expect("the stub engine is uniquely owned")
        .models
        .iter_mut()
    {
        if model.id == "thinking" {
            model.reasoning_default = Some("medium".to_string());
        }
    }
    let engine: Arc<dyn Engine> = stub;

    execute_slash_effects(
        &mut state,
        vec![Effect::ListModels { filter: None }],
        &engine,
        &cmd_tx,
    )
    .await;

    let SlashOverlay::ModelPicker { choices, .. } = &state.slash_overlay else {
        panic!("expected a model picker, got {:?}", state.slash_overlay);
    };
    let shortcut = choices
        .iter()
        .find(|choice| choice.engine_name.is_some())
        .expect("the named engine must appear as a shortcut");
    assert_eq!(
        shortcut.selection.model.reasoning_default,
        Some("medium".to_string()),
        "the shortcut's model must carry the catalog's advertised default effort"
    );
    assert_eq!(
        shortcut.label, "openrouter → stub/thinking  context: 31kb  reasoning: default (medium) ⚙",
        "the shortcut's default slot must name the advertised default"
    );
}

#[tokio::test]
async fn models_lists_an_engine_shortcut_falling_back_to_the_catalogs_context_length() {
    // When the profile's `[engines.<name>]` table doesn't set
    // `context_length`, the shortcut must show the provider's advertised
    // context length for the model instead of silently defaulting.
    let (mut state, _textarea, cmd_tx, _cmd_rx) = setup();
    let engine: Arc<dyn Engine> = StubEngine::with_named_engine(
        &["small", "thinking"],
        "small",
        "openrouter",
        "thinking",
        &["low", "high", "max"],
        Some("max"),
        None,
        Some(999_999),
    );

    execute_slash_effects(
        &mut state,
        vec![Effect::ListModels { filter: None }],
        &engine,
        &cmd_tx,
    )
    .await;

    let SlashOverlay::ModelPicker { choices, .. } = &state.slash_overlay else {
        panic!("expected a model picker, got {:?}", state.slash_overlay);
    };
    let shortcut = choices
        .iter()
        .find(|choice| choice.engine_name.is_some())
        .expect("the named engine must appear as a shortcut");
    assert_eq!(
        shortcut.selection.model.context_length,
        Some(999_999),
        "an unconfigured context_length must fall back to the catalog's advertised value"
    );
    assert_eq!(
        shortcut.label,
        "openrouter → stub/thinking  context: 976kb  reasoning: max ⚙"
    );
}

#[tokio::test]
async fn models_lists_an_engine_shortcut_without_inventing_context_metadata() {
    // When neither TOML nor the catalog advertise a context length, the
    // picker must not present the engine's context fallback as catalog metadata.
    let (mut state, _textarea, cmd_tx, _cmd_rx) = setup();
    let engine: Arc<dyn Engine> = StubEngine::with_named_engine(
        &["small", "thinking"],
        "small",
        "openrouter",
        "thinking",
        &["low", "high", "max"],
        Some("max"),
        None,
        None,
    );

    execute_slash_effects(
        &mut state,
        vec![Effect::ListModels { filter: None }],
        &engine,
        &cmd_tx,
    )
    .await;

    let SlashOverlay::ModelPicker { choices, .. } = &state.slash_overlay else {
        panic!("expected a model picker, got {:?}", state.slash_overlay);
    };
    let shortcut = choices
        .iter()
        .find(|choice| choice.engine_name.is_some())
        .expect("the named engine must appear as a shortcut");
    assert_eq!(
        shortcut.selection.model.context_length, None,
        "an unconfigured, unadvertised context_length must remain unknown in the picker"
    );
}

#[tokio::test]
async fn models_preselects_the_active_model_within_an_initial_filter() {
    let (mut state, _textarea, cmd_tx, _cmd_rx) = setup();
    let engine: Arc<dyn Engine> =
        StubEngine::with_models(&["small", "deepseek-v3", "large"], "large");

    execute_slash_effects(
        &mut state,
        vec![Effect::ListModels {
            filter: Some("DEEPSEEK".to_string()),
        }],
        &engine,
        &cmd_tx,
    )
    .await;

    // The catalog always holds every fetched model — filtering only
    // narrows what is displayed/selectable, so further typing can
    // widen the results again without another provider round-trip.
    let SlashOverlay::ModelPicker {
        choices, filter, ..
    } = &state.slash_overlay
    else {
        panic!(
            "expected a filtered model picker, got {:?}",
            state.slash_overlay
        );
    };
    assert_eq!(
        choices
            .iter()
            .map(|choice| choice.selection.model.id.as_str())
            .collect::<Vec<_>>(),
        vec!["small", "deepseek-v3", "large"]
    );
    assert_eq!(filter, "DEEPSEEK");
    assert_eq!(
        state
            .selected_model()
            .map(|choice| choice.selection.model.id.as_str()),
        Some("deepseek-v3")
    );
}

#[tokio::test]
async fn models_keeps_the_picker_open_when_an_initial_filter_matches_nothing() {
    let (mut state, _textarea, cmd_tx, _cmd_rx) = setup();
    let engine: Arc<dyn Engine> = StubEngine::with_models(&["small", "large"], "large");

    execute_slash_effects(
        &mut state,
        vec![Effect::ListModels {
            filter: Some("missing".to_string()),
        }],
        &engine,
        &cmd_tx,
    )
    .await;

    // No auto-hide/notice on a no-match filter: the picker stays open
    // so the user can keep editing the filter (e.g. backspace) to
    // recover matches, without re-fetching the catalog.
    assert!(matches!(
        state.slash_overlay,
        SlashOverlay::ModelPicker { .. }
    ));
    assert_eq!(state.selected_model(), None);
}

#[tokio::test]
async fn typing_after_models_narrows_the_already_open_picker_without_clearing_the_draft() {
    let (mut state, mut textarea, cmd_tx, _cmd_rx) = setup();
    let engine: Arc<dyn Engine> =
        StubEngine::with_models(&["small", "deepseek-v3", "large"], "large");
    let mut mouse_drag = MouseDrag::default();
    let mut clipboard = NoopClipboard;
    let mut pending_run_ids = PendingRunIds::default();

    // Submit "/models": the picker opens and the draft is normalized to
    // "/models " (not cleared), ready for more typing.
    let effects = submit(&mut state, &mut textarea, "/models");
    let (slash_effects, other_effects) = split_slash_effects(effects);
    execute_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        other_effects,
        &mut mouse_drag,
        &mut clipboard,
        &mut pending_run_ids,
    )
    .await;
    execute_slash_effects(&mut state, slash_effects, &engine, &cmd_tx).await;
    state.refresh_slash_overlay(&textarea.lines().join("\n"));

    assert!(matches!(
        state.slash_overlay,
        SlashOverlay::ModelPicker { .. }
    ));
    assert_eq!(textarea.lines().join("\n"), "/models ");

    // Keep typing: each keystroke must land in the still-open picker's
    // filter, not be dropped or start a disconnected fresh draft.
    for ch in "deepseek".chars() {
        let ctx = textarea_ctx_from(&textarea);
        let effects = presenter::update(
            &mut state,
            TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Char(ch)))),
            &ctx,
        );
        execute_effects(
            &mut state,
            &mut textarea,
            &cmd_tx,
            effects,
            &mut mouse_drag,
            &mut clipboard,
            &mut pending_run_ids,
        )
        .await;
        state.refresh_slash_overlay(&textarea.lines().join("\n"));
    }

    assert_eq!(textarea.lines().join("\n"), "/models deepseek");
    let SlashOverlay::ModelPicker {
        choices, filter, ..
    } = &state.slash_overlay
    else {
        panic!("expected the model picker to stay open while typing a live filter");
    };
    assert_eq!(filter, "deepseek");
    // The full catalog is still cached — only the display/selection is
    // narrowed — so no second provider round-trip was needed.
    assert_eq!(choices.len(), 3);
    assert_eq!(
        state
            .selected_model()
            .map(|choice| choice.selection.model.id.as_str()),
        Some("deepseek-v3")
    );

    // Reported regression: Esc while the (still-open, live-filtered)
    // picker is showing must close the popup AND clear the "/models
    // deepseek" residue from the draft — not leave either behind.
    let ctx = textarea_ctx_from(&textarea);
    let effects = presenter::update(&mut state, TuiEvent::Input(InputEvent::Escape), &ctx);
    execute_effects(
        &mut state,
        &mut textarea,
        &cmd_tx,
        effects,
        &mut mouse_drag,
        &mut clipboard,
        &mut pending_run_ids,
    )
    .await;

    assert_eq!(state.slash_overlay, SlashOverlay::Hidden);
    assert_eq!(textarea.lines().join("\n"), "");
}

#[test]
fn model_picker_cycles_reasoning_effort_with_left_and_right() {
    let (mut state, textarea, _cmd_tx, _cmd_rx) = setup();
    let selection = crate::llm::ProviderModelOption {
        provider_connection: "stub".to_string(),
        model: crate::llm::ModelInfo {
            id: "thinking".to_string(),
            context_length: None,
            reasoning_efforts: Some(vec!["low".to_string(), "high".to_string()]),
            reasoning_default: None,
            input_modalities: None,
        },
        reasoning_effort: None,
    };
    state.slash_overlay = SlashOverlay::ModelPicker {
        choices: vec![ModelChoice {
            label: "stub/thinking  reasoning: default".to_string(),
            initial_reasoning_effort: selection.reasoning_effort.clone(),
            selection,
            is_active: false,
            engine_name: None,
            selection_target: None,
            selection_preview_error: None,
        }],
        filter: String::new(),
        selected: 0,
    };
    let context = textarea_ctx_from(&textarea);

    let effects = presenter::update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Right))),
        &context,
    );
    assert!(effects.is_empty());
    assert_eq!(
        state
            .selected_model()
            .and_then(|choice| choice.selection.reasoning_effort.as_deref()),
        Some("low")
    );
    assert!(state
        .selected_model()
        .expect("selected model")
        .label
        .contains("reasoning: low"));

    presenter::update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Right))),
        &context,
    );
    assert_eq!(
        state
            .selected_model()
            .and_then(|choice| choice.selection.reasoning_effort.as_deref()),
        Some("high")
    );

    presenter::update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Left))),
        &context,
    );
    assert_eq!(
        state
            .selected_model()
            .and_then(|choice| choice.selection.reasoning_effort.as_deref()),
        Some("low")
    );

    presenter::update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Left))),
        &context,
    );
    assert_eq!(
        state
            .selected_model()
            .expect("selected model")
            .selection
            .reasoning_effort,
        None
    );
}

#[tokio::test]
async fn confirming_a_picked_model_switches_the_engine_and_records_the_change() {
    let (mut state, _textarea, cmd_tx, mut cmd_rx) = setup();
    let stub = StubEngine::with_models(&["small", "large"], "large");
    let engine: Arc<dyn Engine> = stub.clone();

    execute_slash_effects(
        &mut state,
        vec![Effect::ListModels { filter: None }],
        &engine,
        &cmd_tx,
    )
    .await;
    // Move the selection off the active model, then confirm it.
    state.move_slash_selection(-1);
    let picked = state
        .selected_model()
        .expect("a highlighted model")
        .selection
        .clone();
    let ctx = TextareaContext {
        is_empty: true,
        text: String::new(),
        cursor_row: 0,
        cursor_row_is_last: true,
    };
    let effects = presenter::update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Enter))),
        &ctx,
    );
    assert_eq!(
        effects,
        vec![
            Effect::ClearTextarea,
            Effect::SwitchModel(ModelSelection::ProviderModel(picked))
        ]
    );
    assert_eq!(state.slash_overlay, SlashOverlay::Hidden);

    execute_slash_effects(&mut state, effects, &engine, &cmd_tx).await;

    assert_eq!(
        *stub.switched_to.lock().expect("stub switch lock"),
        vec!["small".to_string()]
    );
    assert_eq!(engine.config().model, "small");
    // The confirmation is a TUI-only notice — never injected into the
    // conversation (see `Effect::SwitchModel`'s handler for why a
    // mid-conversation `role:"system"` message is unsafe).
    assert!(matches!(
        state.chat_entries.last(),
        Some(crate::tui_reducer_state::ChatEntry::Notice { text }) if text.contains("small")
    ));
    let command = cmd_rx.try_recv().expect("a model-changed command");
    assert!(matches!(
        command,
        AgentCommand::RecordModelChanged { model, .. } if model == "small"
    ));
    assert!(cmd_rx.try_recv().is_err(), "no further commands are sent");
}

#[tokio::test]
async fn selecting_a_named_engine_shortcut_activates_that_exact_engine() {
    use crate::llm::EngineRouter;

    let (mut state, textarea, cmd_tx, _cmd_rx) = setup();
    let engine_a = StubEngine::with_models(&["shared-model"], "shared-model");
    let engine_b = StubEngine::with_models(&["shared-model"], "shared-model");
    {
        let engine_config = &mut *engine_a.config.lock().expect("engine A config lock");
        engine_config.temperature = Some(0.1);
        engine_config.context_length = Some(8_192);
        engine_config.soft_compaction_threshold = 0.6;
    }
    {
        let engine_config = &mut *engine_b.config.lock().expect("engine B config lock");
        engine_config.temperature = Some(0.8);
        engine_config.context_length = Some(32_768);
        engine_config.soft_compaction_threshold = 0.8;
    }
    let router = EngineRouter::new(
        HashMap::from([
            (
                "engine-a".to_string(),
                Arc::clone(&engine_a) as Arc<dyn Engine>,
            ),
            (
                "engine-b".to_string(),
                Arc::clone(&engine_b) as Arc<dyn Engine>,
            ),
        ]),
        "engine-a",
    )
    .expect("router");
    let engine: Arc<dyn Engine> = Arc::new(router);

    execute_slash_effects(
        &mut state,
        vec![Effect::ListModels { filter: None }],
        &engine,
        &cmd_tx,
    )
    .await;

    let SlashOverlay::ModelPicker {
        choices, selected, ..
    } = &state.slash_overlay
    else {
        panic!("expected the model picker, got {:?}", state.slash_overlay);
    };
    assert_eq!(
        choices
            .iter()
            .filter(|choice| choice.is_active)
            .filter_map(|choice| choice.engine_name.as_deref())
            .collect::<Vec<_>>(),
        vec!["engine-a"],
        "only the active named engine is marked active when sibling shortcuts share a model"
    );
    assert_eq!(*selected, 0, "the active engine shortcut is preselected");

    state.move_slash_selection(1);
    assert_eq!(
        state
            .selected_model()
            .and_then(|choice| choice.engine_name.as_deref()),
        Some("engine-b"),
        "the test must select engine B's named shortcut"
    );
    let effects = presenter::update(
        &mut state,
        TuiEvent::Input(InputEvent::KeyInput(key_event(KeyCode::Enter))),
        &textarea_ctx_from(&textarea),
    );
    execute_slash_effects(&mut state, effects, &engine, &cmd_tx).await;

    assert_eq!(
        engine.active_engine_name().as_deref(),
        Some("engine-b"),
        "the exact named shortcut, not a provider representative, must become active"
    );
    assert_eq!(
        engine_a.config().model,
        "shared-model",
        "activating engine B must not mutate engine A"
    );
    assert_eq!(engine_b.config().model, "shared-model");
    assert_eq!(engine.config().temperature, Some(0.8));
    assert_eq!(engine.config().context_length, Some(32_768));
    assert_eq!(engine.config().soft_compaction_threshold, 0.8);
    assert_eq!(engine_a.config().temperature, Some(0.1));
    assert_eq!(engine_a.config().context_length, Some(8_192));
    assert_eq!(engine_a.config().soft_compaction_threshold, 0.6);

    execute_slash_effects(
        &mut state,
        vec![Effect::ListModels { filter: None }],
        &engine,
        &cmd_tx,
    )
    .await;
    let SlashOverlay::ModelPicker {
        choices, selected, ..
    } = &state.slash_overlay
    else {
        panic!(
            "expected the reopened model picker, got {:?}",
            state.slash_overlay
        );
    };
    assert!(
        choices.iter().any(|choice| {
            choice.engine_name.as_deref() == Some("engine-b") && choice.is_active
        }),
        "reopening the picker must mark the active same-model engine by name"
    );
    assert_eq!(
        state
            .selected_model()
            .and_then(|choice| choice.engine_name.as_deref()),
        Some("engine-b"),
        "reopening the picker must preselect the exact active engine"
    );
    assert_eq!(*selected, 1);
}

#[tokio::test]
async fn escape_dismisses_the_model_picker_without_switching() {
    let (mut state, textarea, cmd_tx, _cmd_rx) = setup();
    let stub = StubEngine::with_models(&["small", "large"], "large");
    let engine: Arc<dyn Engine> = stub.clone();
    execute_slash_effects(
        &mut state,
        vec![Effect::ListModels { filter: None }],
        &engine,
        &cmd_tx,
    )
    .await;

    let ctx = textarea_ctx_from(&textarea);
    presenter::update(&mut state, TuiEvent::Input(InputEvent::Escape), &ctx);

    assert_eq!(state.slash_overlay, SlashOverlay::Hidden);
    assert!(stub
        .switched_to
        .lock()
        .expect("stub switch lock")
        .is_empty());
}
