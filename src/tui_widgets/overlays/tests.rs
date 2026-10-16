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

use crate::llm::engine::{CompactionEngineDisplayInfo, EngineDisplayInfo};
use crate::llm::{ModelInfo, ModelSelectionTarget, ProviderModelOption};
use crate::theme::{COMPACTION_MISMATCH_COLOR, MUTED_TEXT_COLOR};
use crate::tui_reducer_state::{ChoiceLabel, ModelChoice, ModelParameterEdit, ParameterField};
use ratatui::style::Color;

const KB: usize = 1024;

fn model_choice(context_length: usize) -> ModelChoice {
    ModelChoice {
        selection: ProviderModelOption {
            provider_connection: "stub".to_string(),
            model: ModelInfo {
                id: "model".to_string(),
                context_length: Some(context_length),
                reasoning_efforts: None,
                reasoning_default: None,
                input_modalities: None,
            },
            reasoning_effort: None,
            context_length_override: None,
        },
        initial_reasoning_effort: None,
        label: ChoiceLabel {
            identity: "stub/model".to_string(),
            details: Vec::new(),
        },
        is_active: false,
        engine_name: None,
        catalog_context_length: Some(context_length),
        selection_target: Some(ModelSelectionTarget {
            engine_name: None,
            provider_connection: "stub".to_string(),
            model: "model".to_string(),
            context_length,
        }),
        selection_preview_error: None,
    }
}

fn picker_overlay(parameters: Option<ModelParameterEdit>) -> SlashOverlay {
    SlashOverlay::ModelPicker {
        choices: vec![model_choice(256 * KB)],
        filter: String::new(),
        selected: 0,
        parameters,
    }
}

fn context_length_draft(
    choice_index: usize,
    context_length: usize,
    edited: bool,
) -> ModelParameterEdit {
    ModelParameterEdit {
        choice_index,
        fields: vec![ParameterField::ContextLength],
        cursor: 0,
        context_length: Some(context_length),
        context_length_maximum: Some(256 * KB),
        context_length_edited: edited,
        reasoning_effort: None,
    }
}

fn engine_display(compaction_engine: CompactionEngineDisplayInfo) -> EngineDisplayInfo {
    EngineDisplayInfo {
        engine_name: String::new(),
        provider_connection: "stub".to_string(),
        model: "model".to_string(),
        reasoning_effort: None,
        compaction_engine: Some(compaction_engine),
        inherits_parent_runtime: false,
    }
}

fn following_compaction_engine() -> CompactionEngineDisplayInfo {
    CompactionEngineDisplayInfo {
        engine_name: String::new(),
        provider_connection: "stub".to_string(),
        model: "model".to_string(),
        context_length: 256 * KB,
        follows_conversation: true,
        shares_conversation: true,
    }
}

fn detail_text_and_color(
    overlay: &SlashOverlay,
    display: &EngineDisplayInfo,
) -> (String, Option<Color>) {
    let lines = model_picker_detail_lines(overlay, display, 120, 5);
    let line = lines.first().expect("compaction detail line");
    let text = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    (text, line.style.fg)
}

#[test]
fn unconfirmed_context_length_edit_previews_in_footer_when_following_conversation() {
    let overlay = picker_overlay(Some(context_length_draft(0, 128 * KB, true)));
    let (text, _) = detail_text_and_color(&overlay, &engine_display(following_compaction_engine()));
    assert_eq!(text, "Compaction uses stub/model, context: 131.1K");
}

#[test]
fn unedited_popup_draft_keeps_stored_window_in_footer() {
    let overlay = picker_overlay(Some(context_length_draft(0, 128 * KB, false)));
    let (text, _) = detail_text_and_color(&overlay, &engine_display(following_compaction_engine()));
    assert_eq!(text, "Compaction uses stub/model, context: 262.1K");
}

#[test]
fn draft_of_another_choice_does_not_change_highlighted_footer() {
    let overlay = picker_overlay(Some(context_length_draft(1, 128 * KB, true)));
    let (text, _) = detail_text_and_color(&overlay, &engine_display(following_compaction_engine()));
    assert_eq!(text, "Compaction uses stub/model, context: 262.1K");
}

#[test]
fn footer_shows_stored_window_once_popup_is_left_without_confirming() {
    let overlay = picker_overlay(None);
    let (text, _) = detail_text_and_color(&overlay, &engine_display(following_compaction_engine()));
    assert_eq!(text, "Compaction uses stub/model, context: 262.1K");
}

#[test]
fn draft_window_matching_explicit_compaction_engine_clears_mismatch_highlight() {
    let explicit_compaction_engine = CompactionEngineDisplayInfo {
        engine_name: "summary".to_string(),
        provider_connection: "other".to_string(),
        model: "small".to_string(),
        context_length: 128 * KB,
        follows_conversation: false,
        shares_conversation: false,
    };
    let display = engine_display(explicit_compaction_engine);

    let mismatching = picker_overlay(None);
    let (_, mismatch_color) = detail_text_and_color(&mismatching, &display);
    assert_eq!(mismatch_color, Some(COMPACTION_MISMATCH_COLOR));

    let matching = picker_overlay(Some(context_length_draft(0, 128 * KB, true)));
    let (text, matching_color) = detail_text_and_color(&matching, &display);
    assert_eq!(text, "Compaction uses other/small, context: 131.1K");
    assert_eq!(matching_color, Some(MUTED_TEXT_COLOR));
}
