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

use crate::agent::AgentCommand;
use crate::llm::engine::{Engine, ModelSelection};
use crate::tui_reducer_state::{
    engine_choice_parts, extract_models_filter, matching_model_indices, model_choice_parts,
    AppState, ModelChoice, SlashOverlay,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// How long `/models` waits for the provider's model list before giving up.
const MODEL_LIST_TIMEOUT: Duration = Duration::from_secs(15);

/// Result of a provider catalog request, including the two failures that need
/// distinct user-facing messages.
pub(super) enum ModelListResult {
    Models(Vec<crate::llm::ProviderModelOption>),
    PartialFailure {
        models: Vec<crate::llm::ProviderModelOption>,
        failures: Vec<String>,
    },
    ProviderError(anyhow::Error),
    Timeout,
}

/// A model catalog request owned by the TUI driver.
pub(super) struct PendingModelList {
    filter: Option<String>,
    task: tokio::task::JoinHandle<ModelListResult>,
}

/// Query one provider catalog without tying the request to the driver's event
/// loop.
pub(super) async fn fetch_model_list(engine: Arc<dyn Engine>) -> ModelListResult {
    match tokio::time::timeout(
        MODEL_LIST_TIMEOUT,
        engine.list_model_options_with_failures(),
    )
    .await
    {
        Ok(Ok((models, failures))) if !failures.is_empty() => {
            ModelListResult::PartialFailure { models, failures }
        }
        Ok(Ok((models, _))) => ModelListResult::Models(models),
        Ok(Err(error)) => ModelListResult::ProviderError(error),
        Err(_) => ModelListResult::Timeout,
    }
}

/// Start a model catalog request, cancelling any older request whose result is
/// no longer relevant to the visible picker.
pub(super) fn start_model_list(
    pending_model_list: &mut Option<PendingModelList>,
    filter: Option<String>,
    engine: &Arc<dyn Engine>,
) {
    abort_pending_model_list(pending_model_list);
    let task = tokio::spawn(fetch_model_list(Arc::clone(engine)));
    *pending_model_list = Some(PendingModelList { filter, task });
}

/// Cancel a model catalog request that is no longer needed by the UI.
pub(super) fn abort_pending_model_list(pending_model_list: &mut Option<PendingModelList>) {
    if let Some(pending) = pending_model_list.take() {
        pending.task.abort();
    }
}

/// Resume a `/models` catalog request that a pause (tool approval, clarify)
/// interrupted.
///
/// A result completing while the pause hid the overlay is discarded — the
/// result branch only applies to a visible `SlashOverlay::LoadingModels` —
/// so once `RestoreInputState` puts `LoadingModels` back, the request must
/// be made again for the picker to ever populate. When the original request
/// is still in flight, its pending handle remains and the existing result
/// branch applies it on arrival, so this is a no-op then.
pub(super) fn resume_interrupted_model_list(
    state: &AppState,
    pending_model_list: &mut Option<PendingModelList>,
    engine: &Arc<dyn Engine>,
    draft: &str,
) {
    if !matches!(state.slash_overlay, SlashOverlay::LoadingModels) {
        return;
    }
    if pending_model_list.is_some() {
        return;
    }
    start_model_list(
        pending_model_list,
        Some(extract_models_filter(draft)),
        engine,
    );
}

/// Wait for the current catalog request, or remain disabled when no request is
/// active so the main `select!` loop can continue handling other events.
pub(super) async fn wait_for_model_list(
    pending_model_list: &mut Option<PendingModelList>,
) -> Option<(Option<String>, ModelListResult)> {
    let Some(pending) = pending_model_list.as_mut() else {
        return std::future::pending().await;
    };
    let filter = pending.filter.clone();
    let result = match (&mut pending.task).await {
        Ok(result) => result,
        Err(error) => {
            ModelListResult::ProviderError(anyhow::anyhow!("model catalog task failed: {error}"))
        }
    };
    Some((filter, result))
}

/// Why some or all provider catalogs did not contribute models.
enum CatalogOutage {
    ProviderError(String),
    PartialFailure(Vec<String>),
    Timeout,
}

impl CatalogOutage {
    fn notice(&self, has_catalog_choices: bool, has_named_engines: bool) -> String {
        let source_summary = match (has_catalog_choices, has_named_engines) {
            (true, _) => "Available catalog choices are retained.",
            (false, true) => "Only configured engines are listed.",
            (false, false) => "No selectable models are available.",
        };
        match self {
            CatalogOutage::ProviderError(error) => {
                format!("Could not list models: {error}. {source_summary}")
            }
            CatalogOutage::PartialFailure(failures) => format!(
                "Some provider connections could not list models: {}. {source_summary}",
                failures.join("; ")
            ),
            CatalogOutage::Timeout => format!(
                "Could not list models: the provider did not answer within {} s. {source_summary}",
                MODEL_LIST_TIMEOUT.as_secs()
            ),
        }
    }
}

/// Apply a completed model catalog request to the visible picker.
///
/// Named-engine shortcuts do not depend on catalog discovery: every shortcut
/// whose model is nonempty and permitted by its connection's allow list stays
/// selectable when the catalog is empty, filtered down to nothing, or
/// unavailable. The picker is only skipped when no shortcut and no catalog
/// model remains, with a notice that tells an outage from a lack of eligible
/// models. A text filter that matches nothing is a separate outcome shown
/// inside the open picker.
pub(super) fn apply_model_list_result(
    state: &mut AppState,
    filter: Option<String>,
    result: ModelListResult,
    engine: &Arc<dyn Engine>,
) {
    let (models, outage) = match result {
        ModelListResult::Models(models) => (models, None),
        ModelListResult::PartialFailure { models, failures } => {
            (models, Some(CatalogOutage::PartialFailure(failures)))
        }
        ModelListResult::ProviderError(error) => (
            Vec::new(),
            Some(CatalogOutage::ProviderError(error.to_string())),
        ),
        ModelListResult::Timeout => (Vec::new(), Some(CatalogOutage::Timeout)),
    };
    let choices = assemble_model_choices(engine, models);
    if choices.is_empty() {
        state.slash_overlay = SlashOverlay::Hidden;
        state.push_notice(match &outage {
            Some(outage) => outage.notice(false, false),
            None => "No eligible models: no configured provider connection reported a model \
                     that its allowed_models setting permits."
                .to_string(),
        });
        return;
    }
    if let Some(outage) = &outage {
        let has_catalog_choices = choices.iter().any(|choice| choice.engine_name.is_none());
        let has_named_engines = choices.iter().any(|choice| choice.engine_name.is_some());
        state.push_notice(outage.notice(has_catalog_choices, has_named_engines));
    }

    let active_config = engine.config();
    let active_engine_name = engine.active_engine_name();
    let filter = filter.unwrap_or_default();
    let selected = matching_model_indices(&choices, &filter)
        .into_iter()
        .position(|index| {
            let choice = &choices[index];
            match choice.engine_name.as_deref() {
                Some(engine_name) => active_engine_name.as_deref() == Some(engine_name),
                None => {
                    choice.selection.provider_connection == active_config.provider_connection
                        && choice.selection.model.id == active_config.model
                }
            }
        })
        .unwrap_or(0);
    state.slash_overlay = SlashOverlay::ModelPicker {
        parameters: None,
        choices,
        filter,
        selected,
    };
}

/// Favorite rank of every `(connection, model id)` pair the engine reports,
/// in configured order per connection. Exact duplicates keep their first
/// position; pairs the allow list forbids are not favorites of any row.
fn favorite_ranks(engine: &Arc<dyn Engine>) -> HashMap<(String, String), usize> {
    let mut favorite_models = engine.favorite_models();
    favorite_models.sort_by(|left, right| left.0.cmp(&right.0));
    let mut ranks = HashMap::new();
    let mut next_rank_by_connection: HashMap<String, usize> = HashMap::new();
    for (provider_connection, model_ids) in favorite_models {
        for model_id in model_ids {
            if engine
                .ensure_provider_model_allowed(&provider_connection, &model_id)
                .is_err()
            {
                continue;
            }
            let identity = (provider_connection.clone(), model_id);
            if ranks.contains_key(&identity) {
                continue;
            }
            let next_rank = next_rank_by_connection
                .entry(provider_connection.clone())
                .or_default();
            ranks.insert(identity, *next_rank);
            *next_rank += 1;
        }
    }
    ranks
}

/// Build every picker row: named-engine shortcuts, then the catalog rows.
/// Display grouping (engines, favorites, remainder) is derived from each
/// row's data by the picker state, so the vector keeps catalog order.
fn assemble_model_choices(
    engine: &Arc<dyn Engine>,
    models: Vec<crate::llm::ProviderModelOption>,
) -> Vec<ModelChoice> {
    let active_config = engine.config();
    let active_reasoning_effort = engine.reasoning_effort();
    let active_engine_name = engine.active_engine_name();
    let favorite_ranks = favorite_ranks(engine);
    let favorite_rank_of = |provider_connection: &str, model_id: &str| {
        favorite_ranks
            .get(&(provider_connection.to_string(), model_id.to_string()))
            .copied()
    };
    // Looked up so an engine shortcut can show the real context length and
    // advertised reasoning efforts of the model it names, rather than
    // reporting "unavailable" for a model the catalog does list.
    let catalog_by_identity: HashMap<(String, String), crate::llm::ModelInfo> = models
        .iter()
        .map(|option| {
            (
                (option.provider_connection.clone(), option.model.id.clone()),
                option.model.clone(),
            )
        })
        .collect();
    // Named engines (explicit `[engines.*]` tables, or their implicit
    // one-per-connection equivalent) are shortcuts to the provider/model/
    // config they name. They stay selectable independent of catalog
    // discovery as long as they name an allowed model.
    let engine_choices = engine
        .named_engine_info()
        .into_iter()
        // An implicit engine without a default model names nothing to select.
        .filter(|named_engine| !named_engine.config.model.is_empty())
        .filter(|named_engine| {
            engine
                .ensure_provider_model_allowed(
                    &named_engine.config.provider_connection,
                    &named_engine.config.model,
                )
                .is_ok()
        })
        .map(|named_engine| {
            let engine_name = named_engine.name;
            let engine_config = named_engine.config;
            let initial_reasoning_effort = named_engine.reasoning_effort;
            let is_active = active_engine_name.as_deref() == Some(&engine_name);
            let catalog_model = catalog_by_identity.get(&(
                engine_config.provider_connection.clone(),
                engine_config.model.clone(),
            ));
            let model = crate::llm::ModelInfo {
                id: engine_config.model.clone(),
                context_length: engine_config
                    .context_length
                    .or_else(|| catalog_model.and_then(|model| model.context_length)),
                reasoning_efforts: catalog_model.and_then(|model| model.reasoning_efforts.clone()),
                reasoning_default: catalog_model.and_then(|model| model.reasoning_default.clone()),
                input_modalities: catalog_model.and_then(|model| model.input_modalities.clone()),
            };
            let favorite_rank = favorite_rank_of(&engine_config.provider_connection, &model.id);
            let selection = crate::llm::ProviderModelOption {
                context_length_override: None,
                provider_connection: engine_config.provider_connection,
                model,
                reasoning_effort: initial_reasoning_effort.clone(),
            };
            let mut choice = ModelChoice {
                label: engine_choice_parts(&engine_name, &selection),
                initial_reasoning_effort,
                selection,
                is_active,
                engine_name: Some(engine_name),
                catalog_context_length: catalog_model.and_then(|model| model.context_length),
                selection_target: None,
                selection_preview_error: None,
                favorite_rank,
            };
            choice.refresh_label();
            choice
        });
    let model_choices = models.into_iter().map(|mut option| {
        let is_active = option.provider_connection == active_config.provider_connection
            && option.model.id == active_config.model;
        if is_active {
            option.reasoning_effort = active_reasoning_effort.clone();
        }
        let favorite_rank = favorite_rank_of(&option.provider_connection, &option.model.id);
        let mut choice = ModelChoice {
            label: model_choice_parts(&option),
            initial_reasoning_effort: option.reasoning_effort.clone(),
            catalog_context_length: option.model.context_length,
            selection: option,
            is_active,
            engine_name: None,
            selection_target: None,
            selection_preview_error: None,
            favorite_rank,
        };
        choice.refresh_label();
        choice
    });
    let mut choices: Vec<ModelChoice> = engine_choices.chain(model_choices).collect();
    for choice in &mut choices {
        match engine.preview_model_selection(&choice.to_model_selection()) {
            Ok(target) => choice.selection_target = Some(target),
            Err(error) => choice.selection_preview_error = Some(error.to_string()),
        }
    }
    choices
}

fn refresh_engine_display_after_selection(
    state: &mut AppState,
    engine: &Arc<dyn Engine>,
    target: &crate::llm::engine::ModelSelectionTarget,
) {
    let runtime_display = crate::llm::engine::EngineDisplayInfo::from_engine(engine.as_ref());
    let mut conversation_display = state.engine_display.clone();
    if !runtime_display.engine_name.is_empty() {
        conversation_display.engine_name = runtime_display.engine_name;
    }
    conversation_display.provider_connection = runtime_display.provider_connection;
    conversation_display.model = runtime_display.model;
    conversation_display.reasoning_effort = runtime_display.reasoning_effort;
    let conversation_engine_name = conversation_display.engine_name.clone();
    let conversation_provider_connection = conversation_display.provider_connection.clone();
    let conversation_model = conversation_display.model.clone();
    refresh_compaction_display(
        &mut conversation_display,
        target,
        &conversation_engine_name,
        &conversation_provider_connection,
        &conversation_model,
    );
    state.engine_display = conversation_display.clone();

    for subagent_display in state.subagent_engine_displays.values_mut() {
        if subagent_display.inherits_parent_runtime {
            subagent_display.engine_name = conversation_display.engine_name.clone();
            subagent_display.provider_connection = conversation_display.provider_connection.clone();
            subagent_display.model = conversation_display.model.clone();
            subagent_display.reasoning_effort = conversation_display.reasoning_effort.clone();
        }
        let subagent_engine_name = subagent_display.engine_name.clone();
        let subagent_provider_connection = subagent_display.provider_connection.clone();
        let subagent_model = subagent_display.model.clone();
        refresh_compaction_display(
            subagent_display,
            target,
            &subagent_engine_name,
            &subagent_provider_connection,
            &subagent_model,
        );
    }
}

fn refresh_compaction_display(
    display: &mut crate::llm::engine::EngineDisplayInfo,
    target: &crate::llm::engine::ModelSelectionTarget,
    conversation_engine_name: &str,
    conversation_provider_connection: &str,
    conversation_model: &str,
) {
    let Some(compaction) = &mut display.compaction_engine else {
        return;
    };
    let selected_summarizer = compaction.shares_conversation
        && (target.engine_name.as_deref() == Some(compaction.engine_name.as_str())
            || (target.engine_name.is_none() && compaction.engine_name.is_empty()));
    if compaction.follows_conversation {
        compaction.engine_name = conversation_engine_name.to_string();
        compaction.provider_connection = conversation_provider_connection.to_string();
        compaction.model = conversation_model.to_string();
        compaction.context_length = target.context_length;
    } else if selected_summarizer {
        compaction.provider_connection = target.provider_connection.clone();
        compaction.model = target.model.clone();
        compaction.context_length = target.context_length;
    }
    compaction.shares_conversation = compaction.follows_conversation
        || (compaction.shares_conversation && compaction.engine_name == conversation_engine_name);
}

pub(super) fn compaction_switch_notice(
    engine_display: &crate::llm::engine::EngineDisplayInfo,
    target: &crate::llm::engine::ModelSelectionTarget,
    selection: &ModelSelection,
) -> Option<String> {
    let compaction = engine_display.compaction_engine.as_ref()?;
    if compaction.follows_conversation {
        // Only a named-engine pick names an engine; a provider-model pick
        // names the model, since the backing engine is an implementation detail.
        return Some(match selection {
            ModelSelection::NamedEngine { engine_name, .. } => {
                format!("Compaction now uses selected engine '{engine_name}'.")
            }
            ModelSelection::ProviderModel(_) => format!(
                "Compaction now uses selected model '{}/{}'.",
                target.provider_connection, target.model
            ),
        });
    }

    let target_is_compaction_engine = compaction.shares_conversation
        && match target.engine_name.as_deref() {
            Some(target_name) if !compaction.engine_name.is_empty() => {
                target_name == compaction.engine_name
            }
            None => compaction.engine_name.is_empty() && compaction.shares_conversation,
            _ => false,
        };
    if target_is_compaction_engine {
        let target_name = target
            .engine_name
            .as_deref()
            .filter(|name| !name.is_empty())
            .unwrap_or(&target.provider_connection);
        Some(format!(
            "Compaction uses '{target_name}', shared with the conversation."
        ))
    } else {
        let compaction_engine_name = if compaction.engine_name.is_empty() {
            format!("{}/{}", compaction.provider_connection, compaction.model)
        } else {
            compaction.engine_name.clone()
        };
        Some(format!("Compaction remains on '{compaction_engine_name}'."))
    }
}

pub(super) async fn switch_model(
    state: &mut AppState,
    selection: &ModelSelection,
    engine: &Arc<dyn Engine>,
    cmd_tx: &mpsc::Sender<AgentCommand>,
) {
    let preview = engine.preview_model_selection(selection);
    match engine.select_model(selection) {
        Ok(()) => {
            let config = engine.config();
            let mut confirmation = match selection {
                ModelSelection::NamedEngine { engine_name, .. } => format!(
                    "Switched to engine '{engine_name}' ({}/{}).",
                    config.provider_connection, config.model
                ),
                ModelSelection::ProviderModel(option) => {
                    match option.model.reasoning_efforts.as_ref() {
                        Some(_) => format!(
                            "Switched to {}/{} (reasoning: {}).",
                            config.provider_connection,
                            config.model,
                            option.reasoning_effort.as_deref().unwrap_or("default")
                        ),
                        None => format!(
                            "Switched to {}/{}.",
                            config.provider_connection, config.model
                        ),
                    }
                }
            };
            match preview {
                Ok(target) => {
                    if let Some(effect) =
                        compaction_switch_notice(&state.engine_display, &target, selection)
                    {
                        confirmation.push(' ');
                        confirmation.push_str(&effect);
                    }
                    refresh_engine_display_after_selection(state, engine, &target);
                }
                Err(error) => {
                    confirmation
                        .push_str(&format!(" Could not resolve compaction effect: {error}."));
                }
            }
            // TUI-only: never injected into the conversation, since a
            // mid-conversation `role:"system"` message is rejected by
            // some providers/chat templates and the switch is already
            // durably recorded below via `RecordModelChanged`.
            state.push_notice(confirmation);
            // Read both values from the live engine because a
            // selection may have changed the active provider as well
            // as its model.
            let _ = cmd_tx
                .send(AgentCommand::RecordModelChanged {
                    provider_connection: config.provider_connection,
                    model: config.model,
                })
                .await;
        }
        Err(error) => {
            state.push_notice(format!(
                "Could not switch to {}/{}: {error}",
                selection.provider_connection(),
                selection.model_id()
            ));
        }
    }
}
