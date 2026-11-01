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
use crate::llm::engine::{ModelSelection, ProviderModelOption, ReasoningEffortChange};
use crate::llm::InputModality;
use crate::tui_reducer_state::model_picker::{
    adjust_context_length, context_length_step, cycle_reasoning_effort, model_parameter_rows,
};
use crate::tui_reducer_state::model_picker::{
    engine_choice_parts, model_choice_parts, model_picker_row_layout, visible_model_choices,
    ChoiceLabel,
};

#[test]
fn matching_model_indices_requires_every_filter_word_in_any_order() {
    let choice =
        |provider_connection: &str, model_id: &str, engine_name: Option<&str>| ModelChoice {
            label: ChoiceLabel {
                identity: format!("{provider_connection}/{model_id}"),
                details: Vec::new(),
            },
            reasoning_options: crate::llm::ReasoningEffortOptions::default(),
            initial_reasoning_effort: None,
            selection: ProviderModelOption {
                context_length_override: None,
                provider_connection: provider_connection.to_string(),
                model: crate::llm::ModelInfo {
                    id: model_id.to_string(),
                    context_length: None,
                    reasoning_evidence: crate::llm::ReasoningEvidence::Unknown,
                    reasoning_efforts: None,
                    reasoning_default: None,
                    input_modalities: None,
                },
                reasoning_effort: None,
            },
            is_active: false,
            engine_name: engine_name.map(str::to_string),
            catalog_context_length: None,
            selection_target: None,
            selection_preview_error: None,
            favorite_rank: None,
        };
    let mut choices = vec![
        choice("openrouter", "deepseek-v3", Some("Remote-Dense")),
        choice("openrouter", "deepseek-v3", Some("remote-fast")),
        choice("openrouter", "deepseek-v3", None),
        choice("ollama", "llama3.2", None),
        choice("stub", "small", None),
    ];
    for choice in &mut choices {
        choice.refresh_label();
    }

    // A multi-word filter must narrow by every word, not the joined
    // phrase as a single substring — each word can match anywhere
    // (provider or model) and in any order.
    assert_eq!(
        matching_model_indices(&choices, "deepseek v3"),
        vec![0, 1, 2],
        "every word of the filter must match, regardless of order"
    );
    assert_eq!(
        matching_model_indices(&choices, "v3 deepseek"),
        vec![0, 1, 2],
        "word order must not matter"
    );
    assert_eq!(
        matching_model_indices(&choices, "openrouter deepseek"),
        vec![0, 1, 2],
        "words may match across the provider and the model id"
    );
    assert_eq!(
        matching_model_indices(&choices, "deepseek missing-word"),
        Vec::<usize>::new(),
        "every word must match somewhere; one unmatched word excludes the choice"
    );
    assert_eq!(
        matching_model_indices(&choices, "   "),
        vec![0, 1, 2, 3, 4],
        "an all-whitespace filter matches everything"
    );
    assert_eq!(
        matching_model_indices(&choices, "DENSE"),
        vec![0],
        "engine names must match case-insensitively without including other engines or catalog rows"
    );
    assert_eq!(
        matching_model_indices(&choices, "v3 REMOTE openrouter"),
        vec![0, 1],
        "words may match across the engine name, provider connection, and model id"
    );
    assert_eq!(
        matching_model_indices(&choices, "dense missing-word"),
        Vec::<usize>::new()
    );
    assert_eq!(
        matching_model_indices(&choices, "unavailable"),
        Vec::<usize>::new(),
        "display metadata must not be searchable"
    );

    let mut state = AppState::new();
    state.slash_overlay = SlashOverlay::ModelPicker {
        parameters: None,
        choices,
        filter: String::new(),
        selected: 4,
    };
    state.refresh_slash_overlay("/models DENSE");
    let selected_choice = state.selected_model().expect("matching engine shortcut");
    assert_eq!(selected_choice.engine_name.as_deref(), Some("Remote-Dense"));
    assert!(matches!(
        selected_choice.to_model_selection(),
        ModelSelection::NamedEngine { engine_name, .. } if engine_name == "Remote-Dense"
    ));
    assert_eq!(
        slash_overlay_text_rows(&state.slash_overlay, PickerRowLayout::SingleRow),
        vec!["Engines".to_string(), selected_choice.label.single_line()]
    );
    state.refresh_slash_overlay("/models remote");
    state.move_slash_selection(1);
    assert_eq!(
        state.selected_model().unwrap().engine_name.as_deref(),
        Some("remote-fast")
    );
}

#[test]
fn refresh_slash_overlay_stays_hidden_while_a_tool_approval_is_pending() {
    let mut state = AppState::new();
    state.refresh_slash_overlay("/mo");
    assert!(
        matches!(state.slash_overlay, SlashOverlay::Suggestions { .. }),
        "expected the suggestion overlay to be open, got {:?}",
        state.slash_overlay
    );

    state.focus = Focus::ToolApproval {
        tool_id: Uuid::new_v4(),
        tool_name: "write".to_string(),
    };
    // The per-action recompute must not reopen the overlay while the
    // approval owns the rows above the input — e.g. while the user types
    // a denial reason containing `/`.
    state.refresh_slash_overlay("/mo");
    assert_eq!(state.slash_overlay, SlashOverlay::Hidden);

    // Once the approval resolves, the recompute derives the overlay from
    // the restored draft again.
    state.focus = Focus::MainInput;
    state.refresh_slash_overlay("/mo");
    assert!(
        matches!(state.slash_overlay, SlashOverlay::Suggestions { .. }),
        "the overlay must reopen from the restored draft, got {:?}",
        state.slash_overlay
    );
}

#[test]
fn refresh_slash_overlay_stays_hidden_while_help_is_open_during_an_outstanding_approval() {
    let mut state = AppState::new();
    // The help popup has taken the focus, but the verdict is still
    // outstanding: the denial reason typed before opening help must not
    // bring the overlay back behind the popup.
    state.focus = Focus::HelpPopup;
    let tool_id = Uuid::new_v4();
    state.chat_entries.push(ChatEntry::ToolBubble {
        id: tool_id,
        name: "write".to_string(),
        state: Box::new(ToolCallState::Pending {
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
        }),
        collapsed: true,
    });
    state.pending_approval_order.push(tool_id);
    state.refresh_slash_overlay("/mo");
    assert_eq!(
        state.slash_overlay,
        SlashOverlay::Hidden,
        "an outstanding approval must suppress the overlay even from the help focus"
    );

    // Once the tool bubble leaves `Pending` the guard lifts with it,
    // even if help is still open (the verdict ends the pause).
    state.chat_entries.clear();
    state.refresh_slash_overlay("/mo");
    assert!(
        matches!(state.slash_overlay, SlashOverlay::Suggestions { .. }),
        "a resolved approval must not keep suppressing the overlay, got {:?}",
        state.slash_overlay
    );
}

#[test]
fn refresh_slash_overlay_keeps_the_selection_while_the_choices_are_unchanged() {
    let mut state = AppState::new();
    state.refresh_slash_overlay("/");
    assert!(matches!(
        state.slash_overlay,
        SlashOverlay::Suggestions { .. }
    ));

    state.move_slash_selection(1);
    assert_eq!(
        state.selected_slash_command().map(|command| command.name),
        Some("/models")
    );

    // Same draft → same choices: the per-action recompute must keep the
    // highlight, otherwise ↑/↓ would reset it after every key press.
    state.refresh_slash_overlay("/");
    assert_eq!(
        state.selected_slash_command().map(|command| command.name),
        Some("/models")
    );

    // A draft that narrows the choice set restarts the selection at the
    // top, mirroring the model picker's filter-change behaviour.
    state.refresh_slash_overlay("/s");
    assert_eq!(
        state.selected_slash_command().map(|command| command.name),
        Some("/session")
    );
    let SlashOverlay::Suggestions { selected, .. } = &state.slash_overlay else {
        panic!("expected suggestions, got {:?}", state.slash_overlay);
    };
    assert_eq!(*selected, 0);
}

#[test]
fn slash_overlay_text_rows_preserves_filtered_model_labels_and_headers() {
    let choice = |label: &str, provider_connection: &str, engine_name: Option<&str>| ModelChoice {
        label: ChoiceLabel {
            identity: label.to_string(),
            details: Vec::new(),
        },
        reasoning_options: crate::llm::ReasoningEffortOptions::default(),
        initial_reasoning_effort: None,
        catalog_context_length: None,
        selection: ProviderModelOption {
            context_length_override: None,
            provider_connection: provider_connection.to_string(),
            model: crate::llm::ModelInfo {
                id: label.to_string(),
                context_length: None,
                reasoning_evidence: crate::llm::ReasoningEvidence::Unknown,
                reasoning_efforts: None,
                reasoning_default: None,
                input_modalities: None,
            },
            reasoning_effort: None,
        },
        is_active: false,
        engine_name: engine_name.map(str::to_string),
        selection_target: None,
        selection_preview_error: None,
        favorite_rank: None,
    };
    let choices = vec![
        choice("engine/model", "engine", Some("engine")),
        choice("provider/model", "provider", None),
    ];

    let filtered_overlay = SlashOverlay::ModelPicker {
        parameters: None,
        choices: choices.clone(),
        filter: "provider".to_string(),
        selected: 0,
    };
    assert_eq!(
        slash_overlay_text_rows(&filtered_overlay, PickerRowLayout::SingleRow),
        vec!["Models".to_string(), "provider/model".to_string()],
        "filtering must keep labels tied to the matching choice index"
    );

    let all_choices_overlay = SlashOverlay::ModelPicker {
        parameters: None,
        choices,
        filter: String::new(),
        selected: 0,
    };
    assert_eq!(
        slash_overlay_text_rows(&all_choices_overlay, PickerRowLayout::SingleRow),
        vec![
            "Engines".to_string(),
            "engine/model".to_string(),
            "Models".to_string(),
            "provider/model".to_string()
        ],
        "group headers contribute their title as non-selectable rows"
    );
}

#[test]
fn model_choice_label_explains_reasoning_effort_availability() {
    let model_option = |reasoning_efforts| ProviderModelOption {
        context_length_override: None,
        provider_connection: "stub".to_string(),
        model: crate::llm::ModelInfo {
            id: "model".to_string(),
            context_length: None,
            reasoning_evidence: crate::llm::ReasoningEvidence::Unknown,
            reasoning_efforts,
            reasoning_default: None,
            input_modalities: None,
        },
        reasoning_effort: None,
    };

    assert_eq!(
        model_choice_parts(&model_option(None)).single_line(),
        "stub/model  reasoning: unavailable"
    );
    assert_eq!(
        model_choice_parts(&model_option(Some(vec![]))).single_line(),
        "stub/model  reasoning: default"
    );
    assert_eq!(
        model_choice_parts(&model_option(Some(vec!["low".to_string()]))).single_line(),
        "stub/model  reasoning: default"
    );
}

#[test]
fn named_engine_choice_preserves_untouched_effort_and_tracks_default_reset() {
    let choice = ModelChoice {
        label: ChoiceLabel {
            identity: "reasoning → stub/model".to_string(),
            details: Vec::new(),
        },
        selection: ProviderModelOption {
            context_length_override: None,
            provider_connection: "stub".to_string(),
            model: crate::llm::ModelInfo {
                id: "model".to_string(),
                context_length: None,
                reasoning_evidence: crate::llm::ReasoningEvidence::Unknown,
                reasoning_efforts: Some(vec!["low".to_string(), "high".to_string()]),
                reasoning_default: None,
                input_modalities: None,
            },
            reasoning_effort: Some("high".to_string()),
        },
        reasoning_options: crate::llm::ReasoningEffortOptions::default(),
        initial_reasoning_effort: Some("high".to_string()),
        is_active: false,
        engine_name: Some("reasoning".to_string()),
        catalog_context_length: None,
        selection_target: None,
        selection_preview_error: None,
        favorite_rank: None,
    };

    assert_eq!(
        choice.to_model_selection(),
        ModelSelection::NamedEngine {
            engine_name: "reasoning".to_string(),
            option: choice.selection.clone(),
            reasoning_effort: ReasoningEffortChange::Unchanged,
        },
        "confirming an untouched named shortcut must retain its Engine runtime effort"
    );

    let mut reset_to_default = choice;
    reset_to_default.selection.reasoning_effort = None;
    assert_eq!(
        reset_to_default.to_model_selection(),
        ModelSelection::NamedEngine {
            engine_name: "reasoning".to_string(),
            option: reset_to_default.selection.clone(),
            reasoning_effort: ReasoningEffortChange::Set(None),
        },
        "cycling an effort back to the provider default must be distinguishable from no edit"
    );
}

#[test]
fn model_choice_label_shows_the_provider_advertised_default_effort() {
    let option = |reasoning_efforts, reasoning_effort| ProviderModelOption {
        context_length_override: None,
        provider_connection: "stub".to_string(),
        model: crate::llm::ModelInfo {
            id: "model".to_string(),
            context_length: None,
            reasoning_evidence: crate::llm::ReasoningEvidence::Unknown,
            reasoning_efforts,
            reasoning_default: Some("medium".to_string()),
            input_modalities: None,
        },
        reasoning_effort,
    };

    assert_eq!(
        model_choice_parts(&option(Some(vec![]), None)).single_line(),
        "stub/model  reasoning: default (medium)"
    );
    assert_eq!(
        model_choice_parts(&option(
            Some(vec!["low".to_string(), "high".to_string()]),
            None
        ))
        .single_line(),
        "stub/model  reasoning: default (medium)",
        "the default slot of a selectable model names the advertised default"
    );
    assert_eq!(
        model_choice_parts(&option(
            Some(vec!["low".to_string(), "medium".to_string()]),
            None
        ))
        .single_line(),
        "stub/model  reasoning: medium (default)",
        "an advertised provider default is named at its own effort"
    );
    assert_eq!(
        model_choice_parts(&option(
            Some(vec!["low".to_string()]),
            Some("low".to_string())
        ))
        .single_line(),
        "stub/model  reasoning: low",
        "an explicit effort must not show the default suffix"
    );
    assert_eq!(
        model_choice_parts(&option(None, None)).single_line(),
        "stub/model  reasoning: unavailable",
        "an unavailable model must ignore the advertised default"
    );
}

fn two_row_fixture_choices() -> Vec<ModelChoice> {
    let choice = |identity: &str,
                  details: &[&str],
                  provider_connection: &str,
                  engine_name: Option<&str>| ModelChoice {
        label: ChoiceLabel {
            identity: identity.to_string(),
            details: details.iter().map(|detail| detail.to_string()).collect(),
        },
        reasoning_options: crate::llm::ReasoningEffortOptions::default(),
        initial_reasoning_effort: None,
        catalog_context_length: None,
        selection: ProviderModelOption {
            context_length_override: None,
            provider_connection: provider_connection.to_string(),
            model: crate::llm::ModelInfo {
                id: "model".to_string(),
                context_length: None,
                reasoning_evidence: crate::llm::ReasoningEvidence::Unknown,
                reasoning_efforts: None,
                reasoning_default: None,
                input_modalities: None,
            },
            reasoning_effort: None,
        },
        is_active: false,
        engine_name: engine_name.map(str::to_string),
        selection_target: None,
        selection_preview_error: None,
        favorite_rank: None,
    };
    vec![
        choice(
            "engine → provider/model",
            &["context: 65.5K", "reasoning: unavailable"],
            "provider",
            Some("engine"),
        ),
        choice(
            "provider/other",
            &["reasoning: unavailable"],
            "provider",
            None,
        ),
    ]
}

#[test]
fn model_picker_row_layout_is_single_row_only_when_every_visible_label_fits() {
    let choices = two_row_fixture_choices();
    let longest_label = choices
        .iter()
        .map(|choice| choice.label.single_line().chars().count())
        .max()
        .expect("fixture has choices");

    assert_eq!(
        model_picker_row_layout(&choices, "", longest_label),
        PickerRowLayout::SingleRow,
        "a label exactly as wide as the budget still fits"
    );
    assert_eq!(
        model_picker_row_layout(&choices, "", longest_label - 1),
        PickerRowLayout::TwoRows,
        "one column short of the longest label needs two rows"
    );
    assert_eq!(
        model_picker_row_layout(&choices, "provider/other", longest_label - 1),
        PickerRowLayout::SingleRow,
        "filtered-out labels must not force two rows"
    );
}

#[test]
fn visible_model_choices_follow_filter_and_display_order() {
    let choices = two_row_fixture_choices();

    let visible: Vec<&str> = visible_model_choices(&choices, "")
        .iter()
        .map(|choice| choice.selection.provider_connection.as_str())
        .collect();
    assert_eq!(visible, vec!["provider", "provider"]);
    assert_eq!(
        visible_model_choices(&choices, "engine")
            .iter()
            .map(|choice| choice.label.single_line())
            .collect::<Vec<_>>(),
        vec!["engine → provider/model  context: 65.5K  reasoning: unavailable".to_string()]
    );
}

#[test]
fn choice_label_keeps_identity_and_details_as_separate_parts() {
    let label = ChoiceLabel {
        identity: "engine → p/m".to_string(),
        details: vec![
            "context: 1.0K".to_string(),
            "reasoning: unavailable".to_string(),
        ],
    };
    assert_eq!(label.identity, "engine → p/m");
    assert_eq!(
        label.details_text(),
        "context: 1.0K  reasoning: unavailable"
    );
    assert_eq!(
        label.single_line(),
        "engine → p/m  context: 1.0K  reasoning: unavailable"
    );
    let bare = ChoiceLabel {
        identity: "p/m".to_string(),
        details: Vec::new(),
    };
    assert_eq!(bare.single_line(), "p/m");
    assert_eq!(bare.details_text(), "");
}

#[test]
fn two_row_layout_repeats_each_entry_and_keeps_the_header_single() {
    use crate::tui_reducer_state::model_picker::PickerGroup::{Engines, Models};
    use crate::tui_reducer_state::model_picker::PickerRow::{Entry, Header};
    let choices = two_row_fixture_choices();

    let single = model_picker_row_entries(&choices, "", PickerRowLayout::SingleRow);
    assert_eq!(
        single,
        vec![Header(Engines), Entry(0), Header(Models), Entry(1)]
    );

    let two_rows = model_picker_row_entries(&choices, "", PickerRowLayout::TwoRows);
    assert_eq!(
        two_rows,
        vec![
            Header(Engines),
            Entry(0),
            Entry(0),
            Header(Models),
            Entry(1),
            Entry(1)
        ]
    );
    assert!(model_picker_row_is_details(&two_rows, 2));
    assert!(!model_picker_row_is_details(&two_rows, 1));
    assert!(!model_picker_row_is_details(&two_rows, 3));
    assert!(!model_picker_row_is_details(&two_rows, 0));
}

#[test]
fn two_row_layout_text_is_identity_then_details_per_entry() {
    let overlay = SlashOverlay::ModelPicker {
        parameters: None,
        choices: two_row_fixture_choices(),
        filter: String::new(),
        selected: 1,
    };

    assert_eq!(
        slash_overlay_text_rows(&overlay, PickerRowLayout::TwoRows),
        vec![
            "Engines".to_string(),
            "engine → provider/model".to_string(),
            "context: 65.5K  reasoning: unavailable".to_string(),
            "Models".to_string(),
            "provider/other".to_string(),
            "reasoning: unavailable".to_string(),
        ]
    );
    assert_eq!(
        slash_overlay_text_rows(&overlay, PickerRowLayout::SingleRow),
        vec![
            "Engines".to_string(),
            "engine → provider/model  context: 65.5K  reasoning: unavailable".to_string(),
            "Models".to_string(),
            "provider/other  reasoning: unavailable".to_string(),
        ]
    );
}

#[test]
fn model_choice_label_keeps_context_and_reasoning_metadata_visible() {
    let option = ProviderModelOption {
        context_length_override: None,
        provider_connection: "openrouter".to_string(),
        model: crate::llm::ModelInfo {
            id: "openai/gpt-oss-120b".to_string(),
            context_length: Some(131_072),
            reasoning_evidence: crate::llm::ReasoningEvidence::Unknown,
            reasoning_efforts: Some(vec!["low".to_string()]),
            reasoning_default: None,
            input_modalities: None,
        },
        reasoning_effort: None,
    };

    assert_eq!(
        model_choice_parts(&option).single_line(),
        "openrouter/openai/gpt-oss-120b  context: 131.1K  reasoning: default"
    );
}

#[test]
fn model_choice_label_formats_context_length_as_compact_decimal_tokens() {
    let model_option = |context_length| ProviderModelOption {
        context_length_override: None,
        provider_connection: "stub".to_string(),
        model: crate::llm::ModelInfo {
            id: "model".to_string(),
            context_length: Some(context_length),
            reasoning_evidence: crate::llm::ReasoningEvidence::Unknown,
            reasoning_efforts: None,
            reasoning_default: None,
            input_modalities: None,
        },
        reasoning_effort: None,
    };

    for (context_length, expected) in [
        (0, "0"),
        (999, "999"),
        (1_000, "1.0K"),
        (1_023, "1.0K"),
        (2_048, "2.0K"),
        (131_072, "131.1K"),
        (1_000_000, "1.0M"),
        (1_048_576, "1.0M"),
    ] {
        assert_eq!(
            model_choice_parts(&model_option(context_length)).single_line(),
            format!("stub/model  context: {expected}  reasoning: unavailable")
        );
    }
}

#[test]
fn model_choice_label_marks_advertised_non_text_input_modalities() {
    let model_option = |input_modalities| ProviderModelOption {
        context_length_override: None,
        provider_connection: "stub".to_string(),
        model: crate::llm::ModelInfo {
            id: "model".to_string(),
            context_length: None,
            reasoning_evidence: crate::llm::ReasoningEvidence::Unknown,
            reasoning_efforts: None,
            reasoning_default: None,
            input_modalities,
        },
        reasoning_effort: None,
    };

    assert_eq!(
        model_choice_parts(&model_option(None)).single_line(),
        "stub/model  reasoning: unavailable",
        "unknown modality data must not add a marker segment"
    );
    assert_eq!(
        model_choice_parts(&model_option(Some(vec![]))).single_line(),
        "stub/model  reasoning: unavailable",
        "a known text-only model must not add a marker segment"
    );
    assert_eq!(
        model_choice_parts(&model_option(Some(vec![InputModality::Image]))).single_line(),
        "stub/model  reasoning: unavailable  [I]"
    );
    assert_eq!(
        model_choice_parts(&model_option(Some(vec![InputModality::File]))).single_line(),
        "stub/model  reasoning: unavailable  [F]"
    );
    assert_eq!(
        model_choice_parts(&model_option(Some(vec![
            InputModality::File,
            InputModality::Image
        ])))
        .single_line(),
        "stub/model  reasoning: unavailable  [IF]",
        "marker order must be fixed regardless of the advertised order"
    );
    assert_eq!(
        engine_choice_parts("engine", &model_option(Some(vec![InputModality::Image])))
            .single_line(),
        "engine → stub/model  reasoning: unavailable  [I]"
    );
}

const KB: usize = 1024;

#[test]
fn context_length_step_is_an_eighth_rounded_to_the_token_step_with_a_floor() {
    assert_eq!(context_length_step(256 * KB), 32 * KB);
    assert_eq!(context_length_step(1024 * KB), 128 * KB);
    assert_eq!(context_length_step(64 * KB), 32 * KB);
    assert_eq!(context_length_step(0), 32 * KB);
}

#[test]
fn adjust_context_length_steps_and_stays_within_the_bounds() {
    assert_eq!(adjust_context_length(256 * KB, 1, None), 288 * KB);
    assert_eq!(adjust_context_length(256 * KB, -1, None), 224 * KB);
    assert_eq!(
        adjust_context_length(256 * KB, 1, Some(260 * KB)),
        260 * KB,
        "the catalog maximum clamps the step"
    );
    assert_eq!(
        adjust_context_length(32 * KB, -1, None),
        32 * KB,
        "the window never drops below one step unit"
    );
}

#[test]
fn adjust_context_length_moves_to_the_next_token_step_from_an_off_grid_value() {
    assert_eq!(adjust_context_length(100 * KB, 1, None), 128 * KB);
    assert_eq!(adjust_context_length(100 * KB, -1, None), 96 * KB);
    assert_eq!(
        adjust_context_length(128 * KB, 1, None),
        160 * KB,
        "after landing on the grid, steps are whole multiples again"
    );
    assert_eq!(adjust_context_length(33 * KB, -1, None), 32 * KB);
}

#[test]
fn cycle_reasoning_effort_stops_at_the_provider_default_and_the_last_effort() {
    let efforts = vec!["low".to_string(), "high".to_string()];
    assert_eq!(
        cycle_reasoning_effort(&efforts, None, None, 1),
        Some("low".to_string())
    );
    assert_eq!(
        cycle_reasoning_effort(&efforts, None, Some("low"), 1),
        Some("high".to_string())
    );
    assert_eq!(
        cycle_reasoning_effort(&efforts, None, Some("high"), 1),
        Some("high".to_string()),
        "the last advertised effort must not wrap back to the provider default"
    );
    assert_eq!(
        cycle_reasoning_effort(&efforts, None, None, -1),
        None,
        "the provider default must not wrap to the last advertised effort"
    );
    assert_eq!(
        cycle_reasoning_effort(&efforts, None, Some("low"), -1),
        None
    );
}

#[test]
fn cycle_reasoning_effort_places_an_advertised_default_at_its_effort() {
    let efforts: Vec<String> = ["none", "low", "medium", "high"]
        .iter()
        .map(|effort| effort.to_string())
        .collect();
    let cycle = |current: Option<&str>, delta| {
        cycle_reasoning_effort(&efforts, Some("medium"), current, delta)
    };
    assert_eq!(
        cycle(Some("low"), 1),
        None,
        "stepping onto the marked medium effort selects the provider default"
    );
    assert_eq!(cycle(None, 1), Some("high".to_string()));
    assert_eq!(cycle(None, -1), Some("low".to_string()));
    assert_eq!(cycle(Some("none"), 1), Some("low".to_string()));
    assert_eq!(
        cycle(Some("none"), -1),
        Some("none".to_string()),
        "the first effort must not wrap"
    );
    assert_eq!(
        cycle(Some("high"), 1),
        Some("high".to_string()),
        "the last effort must not wrap"
    );
    assert_eq!(
        cycle(Some("medium"), 1),
        Some("high".to_string()),
        "an explicit medium effort lands on the marked slot and moves on from there"
    );
}

fn parameter_fixture(
    reasoning_efforts: Option<Vec<String>>,
    context_length: Option<usize>,
) -> AppState {
    let selection = ProviderModelOption {
        provider_connection: "stub".to_string(),
        model: crate::llm::ModelInfo {
            id: "model".to_string(),
            context_length,
            reasoning_evidence: crate::llm::ReasoningEvidence::Unknown,
            reasoning_efforts,
            reasoning_default: Some("medium".to_string()),
            input_modalities: None,
        },
        reasoning_effort: None,
        context_length_override: None,
    };
    let mut state = AppState::new();
    state.slash_overlay = SlashOverlay::ModelPicker {
        choices: vec![ModelChoice {
            label: model_choice_parts(&selection),
            reasoning_options: crate::llm::ReasoningEffortOptions::default(),
            initial_reasoning_effort: None,
            catalog_context_length: context_length,
            selection_target: Some(crate::llm::ModelSelectionTarget {
                engine_name: None,
                provider_connection: "stub".to_string(),
                model: "model".to_string(),
                context_length: 256 * KB,
            }),
            selection,
            is_active: false,
            engine_name: None,
            selection_preview_error: None,
            favorite_rank: None,
        }],
        filter: String::new(),
        selected: 0,
        parameters: None,
    };
    state
}

fn engine_parameter_fixture(
    configured_context_length: usize,
    catalog_context_length: Option<usize>,
) -> AppState {
    let selection = ProviderModelOption {
        provider_connection: "stub".to_string(),
        model: crate::llm::ModelInfo {
            id: "model".to_string(),
            context_length: Some(configured_context_length),
            reasoning_evidence: crate::llm::ReasoningEvidence::Unknown,
            reasoning_efforts: None,
            reasoning_default: None,
            input_modalities: None,
        },
        reasoning_effort: None,
        context_length_override: None,
    };
    let mut state = AppState::new();
    state.slash_overlay = SlashOverlay::ModelPicker {
        choices: vec![ModelChoice {
            label: crate::tui_reducer_state::model_picker::engine_choice_parts(
                "engine", &selection,
            ),
            reasoning_options: crate::llm::ReasoningEffortOptions::default(),
            initial_reasoning_effort: None,
            catalog_context_length,
            selection_target: Some(crate::llm::ModelSelectionTarget {
                engine_name: Some("engine".to_string()),
                provider_connection: "stub".to_string(),
                model: "model".to_string(),
                context_length: configured_context_length,
            }),
            selection,
            is_active: false,
            engine_name: Some("engine".to_string()),
            selection_preview_error: None,
            favorite_rank: None,
        }],
        filter: String::new(),
        selected: 0,
        parameters: None,
    };
    state
}

/// The popup's context window and its cap, or `None` when no popup is open.
fn popup_window(state: &AppState) -> (Option<usize>, Option<usize>) {
    let SlashOverlay::ModelPicker {
        parameters: Some(edit),
        ..
    } = &state.slash_overlay
    else {
        panic!("popup must be open");
    };
    (edit.context_length, edit.context_length_maximum)
}

#[test]
fn parameter_popup_steps_a_configured_engine_window_up_to_the_advertised_window() {
    let mut state = engine_parameter_fixture(32 * KB, Some(512 * KB));
    state.open_model_parameters();
    assert_eq!(
        popup_window(&state),
        (Some(32 * KB), Some(512 * KB)),
        "the popup starts at the configured window and caps at the advertised one"
    );

    state.adjust_model_parameter(1);
    assert_eq!(
        popup_window(&state).0,
        Some(64 * KB),
        "a configured window below the advertised one must step past itself"
    );

    for _ in 0..64 {
        state.adjust_model_parameter(1);
    }
    assert_eq!(
        popup_window(&state).0,
        Some(512 * KB),
        "stepping must reach the advertised window and stop there"
    );
}

#[test]
fn parameter_popup_clamps_a_configured_window_above_the_advertised_one_on_adjust() {
    let mut state = engine_parameter_fixture(1024 * KB, Some(512 * KB));
    state.open_model_parameters();
    assert_eq!(
        popup_window(&state).0,
        Some(1024 * KB),
        "the configured start value is kept until the window is adjusted"
    );

    state.adjust_model_parameter(-1);
    assert_eq!(
        popup_window(&state).0,
        Some(512 * KB),
        "the first adjustment clamps to the advertised window"
    );
}

#[test]
fn parameter_popup_steps_without_a_cap_when_no_window_is_advertised() {
    let mut state = engine_parameter_fixture(32 * KB, None);
    state.open_model_parameters();
    assert_eq!(
        popup_window(&state).1,
        None,
        "no advertised window means no maximum"
    );

    for _ in 0..10 {
        state.adjust_model_parameter(1);
    }
    assert_eq!(popup_window(&state).0, Some(352 * KB));
}

#[test]
fn confirming_an_edited_engine_window_relabels_the_row_with_the_override() {
    let mut state = engine_parameter_fixture(32 * KB, Some(512 * KB));
    state.open_model_parameters();
    state.adjust_model_parameter(1);
    state.confirm_model_parameters();

    let choice = state.selected_model().expect("selected");
    assert_eq!(choice.selection.context_length_override, Some(64 * KB));
    assert!(
        choice.label.details.contains(&"context: 65.5K".to_string()),
        "the row must show the overridden window, got {:?}",
        choice.label.details
    );
}

#[test]
fn parameter_popup_shows_reasoning_only_for_models_with_selectable_efforts() {
    let mut with_efforts = parameter_fixture(Some(vec!["low".to_string()]), Some(512 * KB));
    with_efforts.open_model_parameters();
    let SlashOverlay::ModelPicker {
        parameters: Some(edit),
        ..
    } = &with_efforts.slash_overlay
    else {
        panic!("popup must open");
    };
    assert_eq!(
        edit.fields,
        vec![
            ParameterField::ContextLength,
            ParameterField::ReasoningEffort
        ]
    );
    assert_eq!(
        edit.context_length,
        Some(256 * KB),
        "the preview window is the start value"
    );

    let mut without_efforts = parameter_fixture(Some(Vec::new()), None);
    if let SlashOverlay::ModelPicker { choices, .. } = &mut without_efforts.slash_overlay {
        choices[0].selection_target = None;
    }
    without_efforts.open_model_parameters();
    let SlashOverlay::ModelPicker {
        parameters: Some(edit),
        ..
    } = &without_efforts.slash_overlay
    else {
        panic!("popup must open");
    };
    assert_eq!(edit.fields, vec![ParameterField::ContextLength]);
    assert_eq!(
        edit.context_length, None,
        "no preview and no catalog window is unknown"
    );
}

#[test]
fn confirming_applies_only_edited_parameters_and_escape_discards_them() {
    let mut state = parameter_fixture(Some(vec!["low".to_string()]), Some(512 * KB));
    state.open_model_parameters();
    state.adjust_model_parameter(1);
    state.move_model_parameter_cursor(1);
    state.adjust_model_parameter(1);
    state.leave_model_parameters();
    assert_eq!(
        state
            .selected_model()
            .expect("selected")
            .selection
            .context_length_override,
        None,
        "escape must discard the draft"
    );

    state.open_model_parameters();
    state.adjust_model_parameter(1);
    state.confirm_model_parameters();
    let choice = state.selected_model().expect("selected");
    assert_eq!(
        choice.selection.context_length_override,
        Some(256 * KB + 32 * KB),
        "a confirmed edit becomes the selection's override"
    );
    assert_eq!(
        choice
            .selection_target
            .as_ref()
            .map(|target| target.context_length),
        Some(288 * KB),
        "the compaction preview follows the edited window"
    );
    assert_eq!(choice.selection.reasoning_effort, None);
}

#[test]
fn parameter_rows_show_label_and_value_for_each_field() {
    let mut state = parameter_fixture(Some(vec!["low".to_string()]), Some(512 * KB));
    state.open_model_parameters();
    assert_eq!(
        popup_rows(&state),
        vec![
            "Context length:  ◂ 262.1K ▸".to_string(),
            "Reasoning effort:    default (medium) ▸".to_string(),
        ]
    );
}

#[test]
fn reasoning_row_marks_an_advertised_default_at_its_effort() {
    let mut state = parameter_fixture(
        Some(vec![
            "low".to_string(),
            "medium".to_string(),
            "high".to_string(),
        ]),
        Some(512 * KB),
    );
    state.open_model_parameters();
    state.move_model_parameter_cursor(1);
    assert_eq!(
        popup_rows(&state)[1],
        "Reasoning effort:  ◂ medium (default) ▸",
        "the provider default is marked at its advertised effort"
    );

    state.adjust_model_parameter(-1);
    assert_eq!(
        popup_rows(&state)[1],
        "Reasoning effort:    low ▸",
        "stepping down from the default lands on the effort before it"
    );

    state.adjust_model_parameter(1);
    state.adjust_model_parameter(1);
    assert_eq!(
        popup_rows(&state)[1],
        "Reasoning effort:  ◂ high",
        "stepping past the marked default lands on the next effort"
    );
}

/// Text of every row in the open parameter popup.
fn popup_rows(state: &AppState) -> Vec<String> {
    let SlashOverlay::ModelPicker {
        choices,
        parameters: Some(edit),
        ..
    } = &state.slash_overlay
    else {
        panic!("popup must be open");
    };
    model_parameter_rows(edit, &choices[0])
}

#[test]
fn context_length_row_marks_each_direction_that_can_still_move() {
    let cases = [
        (128 * KB, Some(512 * KB), "Context length:  ◂ 131.1K ▸"),
        (32 * KB, Some(512 * KB), "Context length:    32.8K ▸"),
        (512 * KB, Some(512 * KB), "Context length:  ◂ 524.3K"),
        (128 * KB, None, "Context length:  ◂ 131.1K ▸"),
        (1024 * KB, Some(512 * KB), "Context length:  ◂ 1.0M"),
    ];
    for (configured, catalog, expected) in cases {
        let mut state = engine_parameter_fixture(configured, catalog);
        state.open_model_parameters();
        assert_eq!(
            popup_rows(&state)[0],
            expected,
            "configured {configured} tokens, advertised {catalog:?}"
        );
    }
}

#[test]
fn context_length_row_shows_no_arrows_while_the_window_is_unknown() {
    let mut state = parameter_fixture(Some(Vec::new()), None);
    if let SlashOverlay::ModelPicker { choices, .. } = &mut state.slash_overlay {
        choices[0].selection_target = None;
    }
    state.open_model_parameters();
    assert_eq!(
        popup_rows(&state),
        vec!["Context length:  unknown".to_string()]
    );
}

#[test]
fn reasoning_row_marks_each_direction_that_can_still_move() {
    let mut state = parameter_fixture(
        Some(vec!["low".to_string(), "high".to_string()]),
        Some(512 * KB),
    );
    state.open_model_parameters();
    assert_eq!(
        popup_rows(&state)[1],
        "Reasoning effort:    default (medium) ▸",
        "the provider default can only move greater"
    );

    state.move_model_parameter_cursor(1);
    state.adjust_model_parameter(1);
    assert_eq!(
        popup_rows(&state)[1],
        "Reasoning effort:  ◂ low ▸",
        "an advertised effort in the middle can move both ways"
    );

    state.adjust_model_parameter(1);
    assert_eq!(
        popup_rows(&state)[1],
        "Reasoning effort:  ◂ high",
        "the last advertised effort can only move smaller"
    );
}

fn plain_choice(provider_connection: &str, model_id: &str) -> ModelChoice {
    let selection = ProviderModelOption {
        context_length_override: None,
        provider_connection: provider_connection.to_string(),
        model: crate::llm::ModelInfo {
            id: model_id.to_string(),
            context_length: None,
            reasoning_evidence: crate::llm::ReasoningEvidence::Unknown,
            reasoning_efforts: None,
            reasoning_default: None,
            input_modalities: None,
        },
        reasoning_effort: None,
    };
    ModelChoice {
        label: model_choice_parts(&selection),
        reasoning_options: crate::llm::ReasoningEffortOptions::default(),
        initial_reasoning_effort: None,
        selection,
        is_active: false,
        engine_name: None,
        catalog_context_length: None,
        selection_target: None,
        selection_preview_error: None,
        favorite_rank: None,
    }
}

fn highlighted_pair(state: &AppState) -> (String, String) {
    let (connection, model_id, _) = state
        .highlighted_model_identity()
        .expect("a highlighted model");
    (connection, model_id)
}

#[test]
fn marking_a_favorite_regroups_rows_and_keeps_the_highlighted_identity() {
    let mut state = AppState::new();
    state.slash_overlay = SlashOverlay::ModelPicker {
        choices: vec![
            plain_choice("alpha", "one"),
            plain_choice("alpha", "two"),
            plain_choice("beta", "three"),
        ],
        filter: String::new(),
        selected: 2,
        parameters: None,
    };
    assert_eq!(highlighted_pair(&state), ("beta".into(), "three".into()));

    state.apply_favorite_membership("beta", "three", true);

    assert_eq!(
        highlighted_pair(&state),
        ("beta".into(), "three".into()),
        "the favorite moved to the top group, the highlight followed it"
    );
    let SlashOverlay::ModelPicker {
        choices, selected, ..
    } = &state.slash_overlay
    else {
        panic!("expected a picker");
    };
    assert_eq!(*selected, 0);
    assert!(choices[2].is_favorite());
    assert!(choices[2].label.identity.starts_with("⚑ "));

    state.apply_favorite_membership("alpha", "two", true);
    state.apply_favorite_membership("beta", "three", false);
    let SlashOverlay::ModelPicker { choices, .. } = &state.slash_overlay else {
        panic!("expected a picker");
    };
    assert!(!choices[2].is_favorite());
    assert!(!choices[2].label.identity.starts_with('⚑'));
    assert!(choices[1].is_favorite());
}

#[test]
fn regrouping_keeps_the_parameter_popup_bound_to_its_model() {
    let mut state = AppState::new();
    state.slash_overlay = SlashOverlay::ModelPicker {
        choices: vec![plain_choice("stub", "other"), plain_choice("stub", "model")],
        filter: String::new(),
        selected: 1,
        parameters: None,
    };
    let before = highlighted_pair(&state);
    assert_eq!(before, ("stub".into(), "model".into()));
    let SlashOverlay::ModelPicker {
        choices,
        selected,
        parameters,
        ..
    } = &mut state.slash_overlay
    else {
        panic!("expected a picker");
    };
    let index = matching_model_indices(choices, "")[*selected];
    *parameters = Some(crate::tui_reducer_state::ModelParameterEdit {
        choice_index: index,
        fields: Vec::new(),
        cursor: 0,
        context_length: None,
        context_length_maximum: None,
        context_length_edited: false,
        reasoning_effort: None,
    });

    state.apply_favorite_membership("stub", "model", true);

    assert_eq!(highlighted_pair(&state), before);
    let SlashOverlay::ModelPicker {
        choices,
        parameters: Some(edit),
        ..
    } = &state.slash_overlay
    else {
        panic!("the popup stays open");
    };
    assert_eq!(choices[edit.choice_index].selection.model.id, "model");
}

#[test]
fn favorites_follow_configured_rank_within_sorted_connections_and_filters_apply() {
    let mut choices = vec![
        plain_choice("beta", "b-late"),
        plain_choice("alpha", "a-second"),
        plain_choice("alpha", "a-first"),
        plain_choice("alpha", "plain"),
    ];
    choices[0].favorite_rank = Some(0);
    choices[1].favorite_rank = Some(1);
    choices[2].favorite_rank = Some(0);

    let ordered: Vec<&str> = matching_model_indices(&choices, "")
        .into_iter()
        .map(|index| choices[index].selection.model.id.as_str())
        .collect();
    assert_eq!(ordered, vec!["a-first", "a-second", "b-late", "plain"]);

    let filtered: Vec<&str> = matching_model_indices(&choices, "plain")
        .into_iter()
        .map(|index| choices[index].selection.model.id.as_str())
        .collect();
    assert_eq!(filtered, vec!["plain"]);
    let rows = model_picker_row_entries(&choices, "plain", PickerRowLayout::SingleRow);
    assert_eq!(rows.len(), 2, "only the non-empty group keeps its header");
}

#[test]
fn model_choice_label_preserves_explicit_effort_without_selectable_capability() {
    for reasoning_efforts in [None, Some(Vec::new())] {
        let option = ProviderModelOption {
            provider_connection: "arbitrary-connection".to_string(),
            model: crate::llm::ModelInfo {
                id: "model".to_string(),
                context_length: None,
                reasoning_evidence: crate::llm::ReasoningEvidence::Unknown,
                reasoning_efforts,
                reasoning_default: None,
                input_modalities: None,
            },
            reasoning_effort: Some("medium".to_string()),
            context_length_override: None,
        };
        assert!(model_choice_parts(&option)
            .details_text()
            .contains("reasoning: medium"));
    }
}

#[test]
fn unverified_reasoning_popup_labels_drafts_and_resets_to_default() {
    let mut state = parameter_fixture(Some(Vec::new()), None);
    let SlashOverlay::ModelPicker { choices, .. } = &mut state.slash_overlay else {
        panic!("picker");
    };
    choices[0].selection.model.reasoning_evidence = crate::llm::ReasoningEvidence::Supported;
    choices[0].selection.model.reasoning_default = None;
    choices[0].reasoning_options = crate::llm::ReasoningEffortPolicy::picker_options(
        &crate::llm::edenai_provider::EdenAiReasoningEffortPolicy,
        &choices[0].selection.model,
    );
    state.open_model_parameters();
    assert_eq!(popup_rows(&state)[1], "Reasoning effort:    default ▸");
    state.move_model_parameter_cursor(1);
    state.adjust_model_parameter(1);
    assert!(popup_rows(&state)[1].contains("(unverified)"));
    assert_eq!(
        state.selected_model().unwrap().selection.reasoning_effort,
        None
    );
    state.leave_model_parameters();
    assert_eq!(
        state.selected_model().unwrap().selection.reasoning_effort,
        None
    );
    state.open_model_parameters();
    state.move_model_parameter_cursor(1);
    state.adjust_model_parameter(1);
    state.confirm_model_parameters();
    let choice = state.selected_model().unwrap();
    assert!(choice.selection.reasoning_effort.is_some());
    assert!(choice.label.details_text().contains("(unverified)"));
    assert_eq!(choice.selection.model.reasoning_efforts, Some(Vec::new()));
    state.open_model_parameters();
    state.move_model_parameter_cursor(1);
    state.adjust_model_parameter(-1);
    state.confirm_model_parameters();
    assert_eq!(
        state.selected_model().unwrap().selection.reasoning_effort,
        None
    );
}

#[test]
fn advertised_gateway_choices_stay_strict_and_unknown_choices_remain_unverified() {
    for evidence in [
        crate::llm::ReasoningEvidence::Unknown,
        crate::llm::ReasoningEvidence::Supported,
    ] {
        let mut state = parameter_fixture(None, None);
        let SlashOverlay::ModelPicker { choices, .. } = &mut state.slash_overlay else {
            panic!("picker");
        };
        choices[0].selection.model.reasoning_evidence = evidence;
        choices[0].selection.model.reasoning_default = None;
        choices[0].reasoning_options = crate::llm::ReasoningEffortPolicy::picker_options(
            &crate::llm::edenai_provider::EdenAiReasoningEffortPolicy,
            &choices[0].selection.model,
        );
        state.open_model_parameters();
        state.move_model_parameter_cursor(1);
        state.adjust_model_parameter(1);
        assert_eq!(
            popup_rows(&state)[1],
            "Reasoning effort:  ◂ minimal (unverified) ▸"
        );
    }
    let mut advertised = parameter_fixture(Some(vec!["low".to_string(), "high".to_string()]), None);
    let SlashOverlay::ModelPicker { choices, .. } = &mut advertised.slash_overlay else {
        panic!("picker");
    };
    choices[0].selection.model.reasoning_evidence = crate::llm::ReasoningEvidence::Supported;
    choices[0].selection.model.reasoning_default = None;
    choices[0].reasoning_options = crate::llm::ReasoningEffortPolicy::picker_options(
        &crate::llm::edenai_provider::EdenAiReasoningEffortPolicy,
        &choices[0].selection.model,
    );
    advertised.open_model_parameters();
    advertised.move_model_parameter_cursor(1);
    advertised.adjust_model_parameter(1);
    assert_eq!(popup_rows(&advertised)[1], "Reasoning effort:  ◂ low ▸");
    advertised.adjust_model_parameter(1);
    advertised.adjust_model_parameter(1);
    assert_eq!(popup_rows(&advertised)[1], "Reasoning effort:  ◂ high");
}

#[test]
fn rejected_evidence_does_not_offer_gateway_overrides_but_explicit_effort_can_reset() {
    for evidence in [
        crate::llm::ReasoningEvidence::Unsupported,
        crate::llm::ReasoningEvidence::Invalid,
        crate::llm::ReasoningEvidence::Conflicting,
    ] {
        let mut state = parameter_fixture(None, None);
        let SlashOverlay::ModelPicker { choices, .. } = &mut state.slash_overlay else {
            panic!("picker");
        };
        choices[0].selection.model.reasoning_evidence = evidence;
        choices[0].reasoning_options = crate::llm::ReasoningEffortPolicy::picker_options(
            &crate::llm::edenai_provider::EdenAiReasoningEffortPolicy,
            &choices[0].selection.model,
        );
        state.open_model_parameters();
        assert_eq!(popup_rows(&state).len(), 1);
        state.leave_model_parameters();
        let SlashOverlay::ModelPicker { choices, .. } = &mut state.slash_overlay else {
            panic!("picker");
        };
        choices[0].selection.reasoning_effort = Some("medium".to_string());
        choices[0].initial_reasoning_effort = Some("medium".to_string());
        choices[0].engine_name = Some("named".to_string());
        state.open_model_parameters();
        state.move_model_parameter_cursor(1);
        state.adjust_model_parameter(-1);
        state.confirm_model_parameters();
        assert!(matches!(
            state.selected_model().unwrap().to_model_selection(),
            ModelSelection::NamedEngine {
                reasoning_effort: ReasoningEffortChange::Set(None),
                ..
            }
        ));
    }
}
