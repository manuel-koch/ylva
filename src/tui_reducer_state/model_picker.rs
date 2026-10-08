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

//! Model picker state and operations.

use super::{AppState, BannerDragSelection, Focus};
use crate::llm::engine::{
    ModelSelection, ModelSelectionTarget, ProviderModelOption, ReasoningEffortChange,
};
use crate::llm::InputModality;
use crate::slash_command::{slash_command_suggestions, SlashCommandInfo};

// ─── slash-command overlay ───────────────────────────────────────────────────

/// One selectable entry of the `/models` picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChoice {
    /// Provider connection and model represented by this picker row.
    pub selection: ProviderModelOption,
    /// Engine runtime effort when the picker was populated. Used to distinguish an
    /// untouched named-engine shortcut from an explicit user edit.
    pub initial_reasoning_effort: Option<String>,
    /// Human-readable line shown in the picker.
    pub label: String,
    /// Whether this choice is the model currently serving the conversation.
    pub is_active: bool,
    /// `Some(engine_name)` when this entry is a shortcut to a configured
    /// `[engines.<name>]` table (or its implicit one-per-connection
    /// equivalent), rendered ahead of the raw provider/model catalog and
    /// separated from it by a divider. `None` for a plain provider/model
    /// entry from the catalog itself.
    pub engine_name: Option<String>,
    /// Resolved without mutation through the engine router when the picker
    /// was populated, so the detail area previews the actual routing target.
    pub selection_target: Option<ModelSelectionTarget>,
    /// Reason a selection target could not be resolved for preview.
    pub selection_preview_error: Option<String>,
}

impl ModelChoice {
    /// Convert the highlighted row into an Engine runtime selection without losing
    /// named-engine identity or treating an untouched effort as an override.
    pub fn to_model_selection(&self) -> ModelSelection {
        match &self.engine_name {
            Some(engine_name) => ModelSelection::NamedEngine {
                engine_name: engine_name.clone(),
                option: self.selection.clone(),
                reasoning_effort: if self.selection.reasoning_effort
                    == self.initial_reasoning_effort
                {
                    ReasoningEffortChange::Unchanged
                } else {
                    ReasoningEffortChange::Set(self.selection.reasoning_effort.clone())
                },
            },
            None => ModelSelection::ProviderModel(self.selection.clone()),
        }
    }

    /// Refresh the row text after changing its selected reasoning effort.
    pub fn refresh_label(&mut self) {
        self.label = match &self.engine_name {
            Some(engine_name) => engine_choice_label(engine_name, &self.selection, self.is_active),
            None => model_choice_label(&self.selection, self.is_active),
        };
    }
}

/// Render one `/models` picker row for a plain provider/model catalog entry.
pub(crate) fn model_choice_label(option: &ProviderModelOption, is_active: bool) -> String {
    format_model_choice_label(
        format!("{}/{}", option.provider_connection, option.model.id),
        option,
        is_active,
    )
}

/// Render one `/models` picker row for a named-engine shortcut, prefixed
/// with the engine name so it reads as a shortcut to the provider/model it
/// resolves to.
pub(crate) fn engine_choice_label(
    engine_name: &str,
    option: &ProviderModelOption,
    is_active: bool,
) -> String {
    format_model_choice_label(
        format!(
            "{engine_name} → {}/{}",
            option.provider_connection, option.model.id
        ),
        option,
        is_active,
    )
}

fn format_model_choice_label(
    prefix: String,
    option: &ProviderModelOption,
    is_active: bool,
) -> String {
    let mut label = prefix;
    if let Some(context_length) = option.model.context_length {
        label.push_str(&format!(
            "  context: {}",
            format_context_length(context_length)
        ));
    }
    // The provider-advertised default effort, rendered only in the picker's
    // default slot as `default (X)`.
    let provider_default = option
        .model
        .reasoning_default
        .as_deref()
        .map(|reasoning_default| format!(" ({reasoning_default})"))
        .unwrap_or_default();
    match option.model.reasoning_efforts.as_deref() {
        None => label.push_str("  reasoning: unavailable"),
        Some([]) => label.push_str(&format!("  reasoning: default{provider_default}")),
        Some(_) => match option.reasoning_effort.as_deref() {
            Some(reasoning_effort) => {
                label.push_str(&format!("  reasoning: {reasoning_effort} ⚙"));
            }
            None => {
                label.push_str(&format!("  reasoning: default{provider_default} ⚙"));
            }
        },
    }
    if let Some(modality_markers) =
        format_input_modality_markers(option.model.input_modalities.as_deref())
    {
        label.push_str("  ");
        label.push_str(&modality_markers);
    }
    if is_active {
        label.push_str("  — active");
    }
    label
}

/// Render the bracketed input-modality markers of a `/models` picker row.
///
/// Returns `None` both for unknown modality data (`None`) and for known
/// text-only models (`Some([])`), so a row only gains a segment when the
/// provider advertised a non-text input modality. The letter order is
/// fixed (`I` before `F`) regardless of the advertised order.
fn format_input_modality_markers(input_modalities: Option<&[InputModality]>) -> Option<String> {
    let input_modalities = input_modalities?;
    let mut markers = String::new();
    if input_modalities.contains(&InputModality::Image) {
        markers.push('I');
    }
    if input_modalities.contains(&InputModality::File) {
        markers.push('F');
    }
    if markers.is_empty() {
        None
    } else {
        Some(format!("[{markers}]"))
    }
}
pub(crate) fn format_context_length(context_length: usize) -> String {
    const KILOBYTE: usize = 1024;
    const MEGABYTE: usize = KILOBYTE * 1024;

    if context_length >= MEGABYTE {
        format!("{}mb", context_length / MEGABYTE)
    } else if context_length >= KILOBYTE {
        format!("{}kb", context_length / KILOBYTE)
    } else {
        format!("{context_length} tokens")
    }
}

/// Overlay rendered above the input while a slash command is being composed
/// or resolved.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SlashOverlay {
    /// No slash command is in play.
    #[default]
    Hidden,
    /// The draft looks like a partially typed command; list what it could be.
    Suggestions {
        choices: Vec<SlashCommandInfo>,
        selected: usize,
    },
    /// `/models` was submitted and the provider's list is being fetched.
    LoadingModels,
    /// `/models` returned; the user picks the model to switch to. `choices`
    /// always holds the full fetched catalog — narrowing by `filter` only
    /// changes which indices are *displayed/selectable*, so an in-progress
    /// reasoning-effort edit on a choice survives the user typing more of
    /// the filter afterwards.
    ModelPicker {
        choices: Vec<ModelChoice>,
        filter: String,
        selected: usize,
    },
}

impl SlashOverlay {
    /// Whether the overlay currently captures navigation keys.
    pub fn is_visible(&self) -> bool {
        !matches!(self, SlashOverlay::Hidden)
    }
}

/// The message-draft input state parked by `Effect::SaveInputStateAndClear`
/// while a tool approval or clarify question pauses the input, and given
/// back as one unit by `Effect::RestoreInputState`.
///
/// Bundling the textarea lines with the slash overlay makes the
/// save/restore pairing atomic: a `/models` picker's fetched catalog (which
/// the draft text alone cannot reproduce) always returns together with the
/// draft it was opened from, and neither half can be overwritten without
/// the other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputSnapshot {
    /// The draft's textarea lines as they were; `None` when it was empty.
    pub input_lines: Option<Vec<String>>,
    /// The slash overlay as it was when the input was parked.
    pub slash_overlay: SlashOverlay,
}

/// Indices into `choices` whose engine name and `provider_connection/model` identity
/// contains every whitespace-separated word of `filter`, case-insensitively
/// (in any order — e.g. `"deepseek v3"` matches `stub/deepseek-v3` and
/// `"v3 deepseek"` matches it too). An empty (or all-whitespace) filter
/// matches every choice.
pub(crate) fn matching_model_indices(choices: &[ModelChoice], filter: &str) -> Vec<usize> {
    let filter_words: Vec<String> = filter.split_whitespace().map(str::to_lowercase).collect();
    choices
        .iter()
        .enumerate()
        .filter(|(_, choice)| {
            let identity = format!(
                "{} {}/{}",
                choice.engine_name.as_deref().unwrap_or_default(),
                choice.selection.provider_connection,
                choice.selection.model.id
            )
            .to_lowercase();
            filter_words.iter().all(|word| identity.contains(word))
        })
        .map(|(index, _)| index)
        .collect()
}

/// Render-row mapping for the `/models` picker. `Some(index)` identifies an
/// entry in the filtered choice list; `None` identifies the divider between
/// configured engines and provider models.
pub(crate) fn model_picker_row_entries(
    choices: &[ModelChoice],
    filter: &str,
) -> Vec<Option<usize>> {
    let visible_indices = matching_model_indices(choices, filter);
    let separator_before = visible_indices
        .iter()
        .position(|&index| choices[index].engine_name.is_none())
        .filter(|&boundary| boundary > 0);
    (0..visible_indices.len())
        .flat_map(|index| {
            let separator = (separator_before == Some(index)).then_some(None);
            separator.into_iter().chain(std::iter::once(Some(index)))
        })
        .collect()
}

/// Return the plain text represented by each selectable row in a slash
/// overlay. Model-picker divider rows are represented by an empty string
/// because they have no selectable text.
pub(crate) fn slash_overlay_text_rows(overlay: &SlashOverlay) -> Vec<String> {
    match overlay {
        SlashOverlay::Hidden => Vec::new(),
        SlashOverlay::Suggestions { choices, .. } => choices
            .iter()
            .map(|info| format!("{:<8} {}", info.name, info.description))
            .collect(),
        SlashOverlay::LoadingModels => vec!["Loading models…".to_string()],
        SlashOverlay::ModelPicker {
            choices, filter, ..
        } => {
            let visible_indices = matching_model_indices(choices, filter);
            if visible_indices.is_empty() {
                vec!["Nothing matched your model filter.".to_string()]
            } else {
                model_picker_row_entries(choices, filter)
                    .into_iter()
                    .map(|entry| {
                        entry
                            .and_then(|visible_index| {
                                visible_indices
                                    .get(visible_index)
                                    .and_then(|choice_index| choices.get(*choice_index))
                            })
                            .map(|choice| choice.label.clone())
                            .unwrap_or_default()
                    })
                    .collect()
            }
        }
    }
}

/// The live model-picker filter typed after `/models` in `draft`. Falls
/// back to the whole trimmed draft if it no longer starts with `/models`
/// (e.g. the user backspaced into the command name itself).
pub(crate) fn extract_models_filter(draft: &str) -> String {
    let trimmed = draft.trim_start();
    trimmed
        .strip_prefix("/models")
        .unwrap_or(trimmed)
        .trim()
        .to_string()
}
fn move_selection(selected: &mut usize, choices_length: usize, delta: isize) {
    if choices_length == 0 {
        return;
    }
    let last_index = choices_length - 1;
    let moved = (*selected as isize + delta).clamp(0, last_index as isize);
    *selected = moved as usize;
}

impl AppState {
    /// Recompute the suggestion overlay from the live draft.
    ///
    /// Called by the Driver after every textarea mutation. A visible model
    /// picker outranks suggestions: it was opened by an already-submitted
    /// command and is dismissed by Esc or a selection, not by typing — but
    /// further typing still live-narrows the picker's already-fetched
    /// catalog by re-deriving its filter from the draft.
    ///
    /// A pending tool approval outranks every overlay: the overlay is
    /// parked and hidden by `Effect::SaveInputStateAndClear` when the
    /// approval is armed (see `presenter::tool_bubbles::push_tool_pending_bubble`), and
    /// this recompute must not bring it back while the approval owns the
    /// rows above the input — e.g. while the user types a denial reason
    /// containing `/`. The guard covers both the approval focus itself and
    /// the help popup having temporarily replaced it (consulting the
    /// outstanding-approval bubble via [`AppState::pending_tool_approval`]),
    /// so no slash-command UI can surface while a verdict is outstanding.
    /// `Effect::RestoreInputState` gives the parked overlay back when the
    /// approval resolves; the next recompute then reconciles it with the
    /// restored draft.
    ///
    /// For suggestions, the highlighted entry is kept while the recomputed
    /// choice set is identical to the visible one (so `↑`/`↓` navigation
    /// survives this per-action recompute) and reset to the first entry
    /// when the draft changes the set.
    pub fn refresh_slash_overlay(&mut self, draft: &str) {
        let approval_owns_input = matches!(self.focus, Focus::ToolApproval { .. })
            || (matches!(self.focus, Focus::HelpPopup) && self.pending_tool_approval().is_some());
        if approval_owns_input {
            self.slash_overlay = SlashOverlay::Hidden;
            self.slash_overlay_drag_start = None;
            self.slash_overlay_drag_end = None;
            return;
        }
        if matches!(self.slash_overlay, SlashOverlay::Hidden) {
            self.slash_overlay_drag_start = None;
            self.slash_overlay_drag_end = None;
        }
        if let SlashOverlay::ModelPicker {
            filter, selected, ..
        } = &mut self.slash_overlay
        {
            let new_filter = extract_models_filter(draft);
            if *filter != new_filter {
                *filter = new_filter;
                *selected = 0;
            }
            return;
        }
        if matches!(self.slash_overlay, SlashOverlay::LoadingModels) {
            return;
        }
        let suggestions = slash_command_suggestions(draft);
        if suggestions.is_empty() {
            self.slash_overlay = SlashOverlay::Hidden;
            return;
        }
        // Keep the highlighted entry across the Driver's per-action recompute
        // while the draft leaves the choice set untouched — otherwise `↑`/`↓`
        // would be reset to the first entry after every key press. A draft
        // that narrows or widens the set restarts the selection at the top,
        // mirroring the model picker's filter-change behaviour.
        let selected = match &self.slash_overlay {
            SlashOverlay::Suggestions {
                choices,
                selected: current,
            } if *choices == suggestions => *current,
            _ => 0,
        };
        self.slash_overlay = SlashOverlay::Suggestions {
            choices: suggestions,
            selected,
        };
    }

    /// Return the normalised screen-space range of an active slash-overlay
    /// drag, for rendering its highlight.
    pub fn slash_overlay_drag_selection(&self) -> Option<BannerDragSelection> {
        if !self.slash_overlay.is_visible() {
            return None;
        }
        let (Some((start_col, start_row)), Some((end_col, end_row))) =
            (self.slash_overlay_drag_start, self.slash_overlay_drag_end)
        else {
            return None;
        };
        let ((start_row, start_col), (end_row, end_col)) =
            if (start_row, start_col) <= (end_row, end_col) {
                ((start_row, start_col), (end_row, end_col))
            } else {
                ((end_row, end_col), (start_row, start_col))
            };
        Some(BannerDragSelection {
            start_row,
            start_col,
            end_row,
            end_col,
        })
    }

    /// Move the visible slash overlay's selection by `delta` entries,
    /// saturating at both ends. A no-op unless a selectable overlay is open.
    pub fn move_slash_selection(&mut self, delta: isize) {
        match &mut self.slash_overlay {
            SlashOverlay::Suggestions { choices, selected } => {
                move_selection(selected, choices.len(), delta);
            }
            SlashOverlay::ModelPicker {
                choices,
                filter,
                selected,
            } => {
                let visible_count = matching_model_indices(choices, filter).len();
                move_selection(selected, visible_count, delta);
            }
            _ => {}
        }
    }

    /// Cycle the selected model's reasoning effort, if the model advertises
    /// selectable alternatives. The implicit first state is the provider
    /// default, followed by each advertised effort in order.
    pub fn cycle_selected_model_reasoning_effort(&mut self, delta: isize) {
        let SlashOverlay::ModelPicker {
            choices,
            filter,
            selected,
        } = &mut self.slash_overlay
        else {
            return;
        };
        let Some(&actual_index) = matching_model_indices(choices, filter).get(*selected) else {
            return;
        };
        let choice = &mut choices[actual_index];
        let Some(reasoning_efforts) = choice.selection.model.reasoning_efforts.as_ref() else {
            return;
        };
        if reasoning_efforts.is_empty() {
            return;
        }

        let current_index = choice
            .selection
            .reasoning_effort
            .as_deref()
            .and_then(|effort| {
                reasoning_efforts
                    .iter()
                    .position(|candidate| candidate == effort)
                    .map(|index| index + 1)
            })
            .unwrap_or(0);
        let option_count = reasoning_efforts.len() + 1;
        let next_index =
            (current_index as isize + delta).rem_euclid(option_count as isize) as usize;
        choice.selection.reasoning_effort = if next_index == 0 {
            None
        } else {
            Some(reasoning_efforts[next_index - 1].clone())
        };
        choice.refresh_label();
    }

    /// The command currently highlighted in the suggestion overlay, if any.
    pub fn selected_slash_command(&self) -> Option<SlashCommandInfo> {
        match &self.slash_overlay {
            SlashOverlay::Suggestions { choices, selected } => choices.get(*selected).copied(),
            _ => None,
        }
    }

    /// The model the picker currently highlights, if it is open.
    pub fn selected_model(&self) -> Option<&ModelChoice> {
        match &self.slash_overlay {
            SlashOverlay::ModelPicker {
                choices,
                filter,
                selected,
            } => {
                let actual_index = *matching_model_indices(choices, filter).get(*selected)?;
                choices.get(actual_index)
            }
            _ => None,
        }
    }
}
