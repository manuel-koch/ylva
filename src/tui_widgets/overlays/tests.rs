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
use crate::theme::{muted_text_color, warning_color};
use crate::tui_reducer_state::{
    ChoiceLabel, EngineSaveFocus, EngineSaveForm, ModelChoice, ModelParameterEdit, ParameterField,
};
use ratatui::style::Color;

const KB: usize = 1024;

fn model_choice(context_length: usize) -> ModelChoice {
    ModelChoice {
        configured: None,
        configuration_warnings: Vec::new(),
        selection: ProviderModelOption {
            provider_connection: "stub".to_string(),
            model: ModelInfo {
                pricing: None,
                score: None,
                id: "model".to_string(),
                context_length: Some(context_length),
                max_output_tokens: None,
                reasoning_evidence: crate::llm::ReasoningEvidence::Unknown,
                reasoning_efforts: None,
                reasoning_default: None,
                input_modalities: None,
            },
            reasoning_effort: None,
            context_length_override: None,
            max_output_tokens_override: None,
        },
        reasoning_options: crate::llm::ReasoningEffortOptions::default(),
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
            max_output_tokens: None,
        }),
        selection_preview_error: None,
        favorite_rank: None,
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
        restored_baseline: false,
        initial_context_length: None,
        initial_max_output_tokens: None,
        save_form: None,
        choice_index,
        fields: vec![ParameterField::ContextLength],
        cursor: 0,
        context_length: Some(context_length),
        context_length_maximum: Some(256 * KB),
        context_length_edited: edited,
        max_output_tokens: None,
        max_output_tokens_maximum: None,
        max_output_tokens_edited: false,
        reasoning_effort: None,
    }
}

fn engine_display(compaction_engine: CompactionEngineDisplayInfo) -> EngineDisplayInfo {
    EngineDisplayInfo {
        config_modified: false,
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
        config_modified: false,
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
    let line = lines
        .iter()
        .find(|line| line.to_string().starts_with("Compaction"))
        .expect("compaction detail line");
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
fn narrow_width_wraps_the_details_instead_of_dropping_them() {
    let overlay = picker_overlay(Some(context_length_draft(0, 128 * KB, true)));
    let display = engine_display(following_compaction_engine());
    let lines = model_picker_detail_lines(&overlay, &display, 20, 10);
    assert!(lines.len() > 1);
    let rows: Vec<String> = lines.iter().map(ToString::to_string).collect();
    assert!(rows.iter().all(|row| row.chars().count() <= 20), "{rows:?}");
    let joined = rows.join(" ");
    assert!(
        joined.ends_with("Compaction uses stub/model, context: 131.1K"),
        "{joined}"
    );
}

#[test]
fn details_spell_out_the_selected_identity_and_parameters() {
    let mut overlay = picker_overlay(None);
    if let SlashOverlay::ModelPicker { choices, .. } = &mut overlay {
        choices[0].engine_name = Some("fast".to_string());
        choices[0].label.details = vec!["context: 256K".to_string(), "reasoning: low".to_string()];
    }
    let display = engine_display(following_compaction_engine());
    let rows: Vec<String> = model_picker_detail_lines(&overlay, &display, 120, 10)
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(rows[0], "engine: fast");
    assert_eq!(rows[1], "provider: stub");
    assert_eq!(rows[2], "model: model");
    assert!(rows[3].starts_with("context: 256K"), "{rows:?}");
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
        config_modified: false,
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
    assert_eq!(mismatch_color, Some(warning_color()));

    let matching = picker_overlay(Some(context_length_draft(0, 128 * KB, true)));
    let (text, matching_color) = detail_text_and_color(&matching, &display);
    assert_eq!(text, "Compaction uses other/small, context: 131.1K");
    assert_eq!(matching_color, Some(muted_text_color()));
}

#[test]
fn model_name_after_the_last_slash_is_bold_in_the_identity() {
    let mut choice = model_choice(8 * KB);
    choice.selection.model.id = "anthropic/claude-haiku-5.5".to_string();
    let text = "conn/anthropic/claude-haiku-5.5 *  context: claude-haiku-5.5";
    choice.label.identity = "conn/anthropic/claude-haiku-5.5 *".to_string();

    let line = model_name_emphasized_line("> ", text, &choice, Style::default()).expect("found");

    let bold: Vec<&str> = line
        .spans
        .iter()
        .filter(|span| span.style.add_modifier.contains(Modifier::BOLD))
        .map(|span| span.content.as_ref())
        .collect();
    assert_eq!(bold, vec!["claude-haiku-5.5"]);
    assert_eq!(line.spans[0].content, "> conn/anthropic/");
    assert_eq!(
        line.spans[2].content, " *  context: claude-haiku-5.5",
        "details stay plain"
    );

    let elided = model_name_emphasized_line("> ", "conn/anth…", &choice, Style::default());
    assert!(elided.is_none());
}

fn render_overlay(overlay: &SlashOverlay, width: u16) -> (SlashOverlayLayout, String) {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut terminal = Terminal::new(TestBackend::new(width, 24)).expect("terminal");
    let mut layout = SlashOverlayLayout::default();
    let display = engine_display(following_compaction_engine());
    terminal
        .draw(|frame| {
            let input_area = Rect::new(0, 20, width, 3);
            draw_slash_overlay(
                frame,
                input_area,
                overlay,
                None,
                &display,
                &mut layout,
                None,
            );
        })
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    let text = (0..24)
        .map(|row| {
            (0..width)
                .map(|column| buffer[(column, row)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    (layout, text)
}

fn parameters_with_buttons(save_form: Option<Box<EngineSaveForm>>) -> ModelParameterEdit {
    let mut edit = context_length_draft(0, 128 * KB, false);
    edit.fields = vec![
        ParameterField::ContextLength,
        ParameterField::Apply,
        ParameterField::Save,
    ];
    edit.save_form = save_form;
    edit
}

#[test]
fn parameter_buttons_share_a_row_when_wide_and_wrap_when_narrow() {
    let overlay = picker_overlay(Some(parameters_with_buttons(None)));
    let rows_at = |width: u16| {
        let (layout, _) = render_overlay(&overlay, width);
        assert_eq!(layout.parameter_buttons.len(), 2, "width {width}");
        for (area, _) in &layout.parameter_buttons {
            assert!(area.right() <= width, "hit area inside the terminal");
        }
        let mut rows: Vec<u16> = layout.parameter_buttons.iter().map(|(a, _)| a.y).collect();
        rows.dedup();
        rows.len()
    };
    let (wide, _) = render_overlay(&overlay, 80);
    assert_eq!(
        wide.parameter_row_entries,
        [0, 1],
        "rows map to leading entries"
    );
    let (narrow, _) = render_overlay(&overlay, 24);
    assert_eq!(narrow.parameter_row_entries, [0, 1, 2]);
    assert_eq!(rows_at(80), 1, "both buttons fit one row");
    assert_eq!(rows_at(24), 2, "one button per row when narrow");
}

#[test]
fn save_page_shows_its_controls_and_clipped_hit_areas_at_every_width() {
    let form = EngineSaveForm {
        update: false,
        update_available: false,
        name: "ünïcödé-名前-very-long-engine-name".to_string(),
        name_cursor: 0,
        focus: EngineSaveFocus::Name,
        error: None,
        pending: false,
        origin_name: None,
        preview: None,
        name_problem: None,
    };
    let overlay = picker_overlay(Some(parameters_with_buttons(Some(Box::new(form)))));
    for width in [80, 60, 40, 24] {
        let (layout, text) = render_overlay(&overlay, width);
        let focuses: Vec<_> = layout
            .engine_save_controls
            .iter()
            .map(|(_, focus)| *focus)
            .collect();
        assert!(focuses.contains(&EngineSaveFocus::Name), "width {width}");
        assert!(focuses.contains(&EngineSaveFocus::Save), "width {width}");
        assert!(
            focuses.contains(&EngineSaveFocus::SaveAndApply),
            "width {width}"
        );
        assert!(text.contains("Save engine"), "width {width}: {text}");
        for (area, _) in &layout.engine_save_controls {
            assert!(area.right() <= width, "hit area clipped at width {width}");
        }
    }
}

#[test]
fn save_page_marks_the_name_caret_with_a_highlighted_cell_on_the_character_under_it() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let form = EngineSaveForm {
        update: false,
        update_available: false,
        name: "fast".to_string(),
        name_cursor: 1,
        focus: EngineSaveFocus::Name,
        error: None,
        pending: false,
        origin_name: None,
        preview: None,
        name_problem: None,
    };
    let overlay = picker_overlay(Some(parameters_with_buttons(Some(Box::new(form)))));
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
    let mut layout = SlashOverlayLayout::default();
    let display = engine_display(following_compaction_engine());
    terminal
        .draw(|frame| {
            let input_area = Rect::new(0, 20, 80, 3);
            draw_slash_overlay(
                frame,
                input_area,
                &overlay,
                None,
                &display,
                &mut layout,
                None,
            );
        })
        .expect("draw");
    let buffer = terminal.backend().buffer();
    let reversed: Vec<&str> = buffer
        .content()
        .iter()
        .filter(|cell| cell.bg == crate::theme::list_selection_foreground_color())
        .map(|cell| cell.symbol())
        .collect();
    assert_eq!(reversed, ["a"]);
}

#[test]
fn long_save_page_info_scrolls_below_the_controls_instead_of_squeezing_them() {
    let form = EngineSaveForm {
        update: false,
        update_available: false,
        name: "fast".to_string(),
        name_cursor: 4,
        focus: EngineSaveFocus::Name,
        error: Some("a very long failure explanation ".repeat(40)),
        pending: false,
        origin_name: None,
        preview: None,
        name_problem: None,
    };
    let overlay = picker_overlay(Some(parameters_with_buttons(Some(Box::new(form)))));
    let (layout, text) = render_overlay(&overlay, 60);
    assert!(text.contains("Name:  fast"), "{text}");
    assert!(text.contains("Save"), "{text}");
    assert!(layout.detail_total_rows > layout.detail_rows.len());
    assert_eq!(layout.detail_first_visible_row, 0);
    assert!(layout
        .engine_save_controls
        .iter()
        .any(|(_, focus)| *focus == EngineSaveFocus::SaveAndApply));

    let mut scrolled = SlashOverlayLayout {
        detail_first_visible_row: 2,
        ..layout
    };
    let display = engine_display(following_compaction_engine());
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 24)).expect("terminal");
    terminal
        .draw(|frame| {
            let input_area = Rect::new(0, 20, 60, 3);
            draw_slash_overlay(
                frame,
                input_area,
                &overlay,
                None,
                &display,
                &mut scrolled,
                None,
            );
        })
        .expect("draw");
    assert_eq!(scrolled.detail_first_visible_row, 2);
    let details = scrolled.detail_area;
    let scrollbar_column = details.right() - 1;
    let buffer = terminal.backend().buffer();
    let glyphs: Vec<String> = (details.y..details.bottom())
        .map(|row| buffer[(scrollbar_column, row)].symbol().to_string())
        .collect();
    assert!(
        glyphs.iter().any(|glyph| glyph == "┃" || glyph == "│"),
        "a scroll indicator is drawn in the details' last column: {glyphs:?}"
    );
}

#[test]
fn model_list_window_is_stable_when_a_header_and_entry_do_not_fit_together() {
    let mut favorite = model_choice(256 * KB);
    favorite.favorite_rank = Some(0);
    let overlay_for = |selected: usize| SlashOverlay::ModelPicker {
        choices: vec![
            favorite.clone(),
            model_choice(128 * KB),
            model_choice(64 * KB),
        ],
        filter: String::new(),
        selected,
        parameters: None,
    };
    let display = engine_display(following_compaction_engine());
    for input_row in 3..12 {
        for selected in 0..3 {
            let overlay = overlay_for(selected);
            let mut layout = SlashOverlayLayout::default();
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24))
                .expect("terminal");
            let mut windows = Vec::new();
            for _ in 0..4 {
                terminal
                    .draw(|frame| {
                        let input_area = Rect::new(0, input_row, 80, 3);
                        draw_slash_overlay(
                            frame,
                            input_area,
                            &overlay,
                            None,
                            &display,
                            &mut layout,
                            None,
                        );
                    })
                    .expect("draw");
                windows.push(layout.first_visible_row);
            }
            assert!(
                windows.windows(2).all(|pair| pair[0] == pair[1]),
                "window flips between frames: {windows:?} (input row {input_row}, selected {selected})"
            );
        }
    }
}

#[test]
fn narrow_terminal_shows_resize_guidance_and_the_page_returns_when_widened() {
    let overlay = picker_overlay(Some(parameters_with_buttons(None)));

    let (narrow, text) = render_overlay(&overlay, 16);
    assert!(text.contains("Widen"), "{text}");
    assert!(narrow.parameter_buttons.is_empty(), "no stale hit areas");

    let (wide, text) = render_overlay(&overlay, 40);
    assert!(text.contains("Save…"), "{text}");
    assert_eq!(wide.parameter_buttons.len(), 2);
    let SlashOverlay::ModelPicker {
        parameters: Some(edit),
        ..
    } = &overlay
    else {
        unreachable!()
    };
    assert_eq!(edit.cursor, 0, "focus is untouched by drawing");
}

#[test]
fn model_picker_dialog_height_does_not_depend_on_the_selected_details() {
    let overlay_for = |selected: usize, filter: &str| SlashOverlay::ModelPicker {
        choices: (0..30).map(|_| model_choice(256 * KB)).collect(),
        filter: filter.to_string(),
        selected,
        parameters: None,
    };
    let dialog_height = |text: &str| text.lines().filter(|line| !line.trim().is_empty()).count();
    let (_, first) = render_overlay(&overlay_for(0, ""), 60);
    let (_, later) = render_overlay(&overlay_for(5, ""), 60);
    assert_eq!(dialog_height(&first), dialog_height(&later));
}
