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

#[test]
fn matching_model_indices_requires_every_filter_word_in_any_order() {
    let choice = |provider_connection: &str, model_id: &str| ModelChoice {
        label: format!("{provider_connection}/{model_id}"),
        initial_reasoning_effort: None,
        selection: ProviderModelOption {
            provider_connection: provider_connection.to_string(),
            model: crate::llm::ModelInfo {
                id: model_id.to_string(),
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
    };
    let choices = vec![
        choice("openrouter", "deepseek-v3"),
        choice("ollama", "llama3.2"),
        choice("stub", "small"),
    ];

    // A multi-word filter must narrow by every word, not the joined
    // phrase as a single substring — each word can match anywhere
    // (provider or model) and in any order.
    assert_eq!(
        matching_model_indices(&choices, "deepseek v3"),
        vec![0],
        "every word of the filter must match, regardless of order"
    );
    assert_eq!(
        matching_model_indices(&choices, "v3 deepseek"),
        vec![0],
        "word order must not matter"
    );
    assert_eq!(
        matching_model_indices(&choices, "openrouter deepseek"),
        vec![0],
        "words may match across the provider and the model id"
    );
    assert_eq!(
        matching_model_indices(&choices, "deepseek missing-word"),
        Vec::<usize>::new(),
        "every word must match somewhere; one unmatched word excludes the choice"
    );
    assert_eq!(
        matching_model_indices(&choices, "   "),
        vec![0, 1, 2],
        "an all-whitespace filter matches everything"
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
    state.chat_entries.push(ChatEntry::ToolBubble {
        id: Uuid::new_v4(),
        name: "write".to_string(),
        state: Box::new(ToolCallState::Pending {
            arguments: serde_json::json!({}),
            content_sections: vec![],
            title_arguments: vec![],
        }),
        collapsed: true,
    });
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
    state.refresh_slash_overlay("/t");
    assert_eq!(
        state.selected_slash_command().map(|command| command.name),
        Some("/tools")
    );
    let SlashOverlay::Suggestions { selected, .. } = &state.slash_overlay else {
        panic!("expected suggestions, got {:?}", state.slash_overlay);
    };
    assert_eq!(*selected, 0);
}

#[test]
fn slash_overlay_text_rows_preserves_filtered_model_labels_and_divider() {
    let choice = |label: &str, provider_connection: &str, engine_name: Option<&str>| ModelChoice {
        label: label.to_string(),
        initial_reasoning_effort: None,
        selection: ProviderModelOption {
            provider_connection: provider_connection.to_string(),
            model: crate::llm::ModelInfo {
                id: label.to_string(),
                context_length: None,
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
    };
    let choices = vec![
        choice("engine/model", "engine", Some("engine")),
        choice("provider/model", "provider", None),
    ];

    let filtered_overlay = SlashOverlay::ModelPicker {
        choices: choices.clone(),
        filter: "provider".to_string(),
        selected: 0,
    };
    assert_eq!(
        slash_overlay_text_rows(&filtered_overlay),
        vec!["provider/model".to_string()],
        "filtering must keep labels tied to the matching choice index"
    );

    let all_choices_overlay = SlashOverlay::ModelPicker {
        choices,
        filter: String::new(),
        selected: 0,
    };
    assert_eq!(
        slash_overlay_text_rows(&all_choices_overlay),
        vec![
            "engine/model".to_string(),
            String::new(),
            "provider/model".to_string()
        ],
        "the divider must remain an empty non-selectable row"
    );
}

#[test]
fn model_choice_label_explains_reasoning_effort_availability() {
    let model_option = |reasoning_efforts| ProviderModelOption {
        provider_connection: "stub".to_string(),
        model: crate::llm::ModelInfo {
            id: "model".to_string(),
            context_length: None,
            reasoning_efforts,
            reasoning_default: None,
            input_modalities: None,
        },
        reasoning_effort: None,
    };

    assert_eq!(
        model_choice_label(&model_option(None), false),
        "stub/model  reasoning: unavailable"
    );
    assert_eq!(
        model_choice_label(&model_option(Some(vec![])), false),
        "stub/model  reasoning: default"
    );
    assert_eq!(
        model_choice_label(&model_option(Some(vec!["low".to_string()])), false),
        "stub/model  reasoning: default ⚙"
    );
}

#[test]
fn named_engine_choice_preserves_untouched_effort_and_tracks_default_reset() {
    let choice = ModelChoice {
        label: "reasoning → stub/model".to_string(),
        selection: ProviderModelOption {
            provider_connection: "stub".to_string(),
            model: crate::llm::ModelInfo {
                id: "model".to_string(),
                context_length: None,
                reasoning_efforts: Some(vec!["low".to_string(), "high".to_string()]),
                reasoning_default: None,
                input_modalities: None,
            },
            reasoning_effort: Some("high".to_string()),
        },
        initial_reasoning_effort: Some("high".to_string()),
        is_active: false,
        engine_name: Some("reasoning".to_string()),
        selection_target: None,
        selection_preview_error: None,
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
        provider_connection: "stub".to_string(),
        model: crate::llm::ModelInfo {
            id: "model".to_string(),
            context_length: None,
            reasoning_efforts,
            reasoning_default: Some("medium".to_string()),
            input_modalities: None,
        },
        reasoning_effort,
    };

    assert_eq!(
        model_choice_label(&option(Some(vec![]), None), false),
        "stub/model  reasoning: default (medium)"
    );
    assert_eq!(
        model_choice_label(
            &option(Some(vec!["low".to_string(), "high".to_string()]), None),
            false
        ),
        "stub/model  reasoning: default (medium) ⚙",
        "the default slot of a selectable model names the advertised default"
    );
    assert_eq!(
        model_choice_label(
            &option(Some(vec!["low".to_string()]), Some("low".to_string())),
            false
        ),
        "stub/model  reasoning: low ⚙",
        "an explicit effort must not show the default suffix"
    );
    assert_eq!(
        model_choice_label(&option(None, None), false),
        "stub/model  reasoning: unavailable",
        "an unavailable model must ignore the advertised default"
    );
}

#[test]
fn model_choice_label_keeps_context_and_reasoning_metadata_visible() {
    let option = ProviderModelOption {
        provider_connection: "openrouter".to_string(),
        model: crate::llm::ModelInfo {
            id: "openai/gpt-oss-120b".to_string(),
            context_length: Some(131_072),
            reasoning_efforts: Some(vec!["low".to_string()]),
            reasoning_default: None,
            input_modalities: None,
        },
        reasoning_effort: None,
    };

    assert_eq!(
        model_choice_label(&option, false),
        "openrouter/openai/gpt-oss-120b  context: 128kb  reasoning: default ⚙"
    );
}

#[test]
fn model_choice_label_formats_context_length_as_integer_binary_size() {
    let model_option = |context_length| ProviderModelOption {
        provider_connection: "stub".to_string(),
        model: crate::llm::ModelInfo {
            id: "model".to_string(),
            context_length: Some(context_length),
            reasoning_efforts: None,
            reasoning_default: None,
            input_modalities: None,
        },
        reasoning_effort: None,
    };

    assert_eq!(
        model_choice_label(&model_option(2_048), false),
        "stub/model  context: 2kb  reasoning: unavailable"
    );
    assert_eq!(
        model_choice_label(&model_option(1_048_576), false),
        "stub/model  context: 1mb  reasoning: unavailable"
    );
    assert_eq!(
        model_choice_label(&model_option(1_023), false),
        "stub/model  context: 1023 tokens  reasoning: unavailable"
    );
}

#[test]
fn model_choice_label_marks_advertised_non_text_input_modalities() {
    let model_option = |input_modalities| ProviderModelOption {
        provider_connection: "stub".to_string(),
        model: crate::llm::ModelInfo {
            id: "model".to_string(),
            context_length: None,
            reasoning_efforts: None,
            reasoning_default: None,
            input_modalities,
        },
        reasoning_effort: None,
    };

    assert_eq!(
        model_choice_label(&model_option(None), false),
        "stub/model  reasoning: unavailable",
        "unknown modality data must not add a marker segment"
    );
    assert_eq!(
        model_choice_label(&model_option(Some(vec![])), false),
        "stub/model  reasoning: unavailable",
        "a known text-only model must not add a marker segment"
    );
    assert_eq!(
        model_choice_label(&model_option(Some(vec![InputModality::Image])), true),
        "stub/model  reasoning: unavailable  [I]  — active"
    );
    assert_eq!(
        model_choice_label(&model_option(Some(vec![InputModality::File])), false),
        "stub/model  reasoning: unavailable  [F]"
    );
    assert_eq!(
        model_choice_label(
            &model_option(Some(vec![InputModality::File, InputModality::Image])),
            false
        ),
        "stub/model  reasoning: unavailable  [IF]",
        "marker order must be fixed regardless of the advertised order"
    );
    assert_eq!(
        engine_choice_label(
            "engine",
            &model_option(Some(vec![InputModality::Image])),
            false
        ),
        "engine → stub/model  reasoning: unavailable  [I]"
    );
}
