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
    /// Row text shown in the picker, kept as its identity and detail segments.
    pub label: ChoiceLabel,
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
            Some(engine_name) => engine_choice_parts(engine_name, &self.selection),
            None => model_choice_parts(&self.selection),
        };
    }
}

/// Separator drawn between the segments of a single-line row. Presentation
/// only: rows keep their segments as data and never parse them back out.
const DETAIL_SEGMENT_SEPARATOR: &str = "  ";

/// One picker row's text as raw parts: the identity it names and the detail
/// segments shown after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChoiceLabel {
    pub identity: String,
    pub details: Vec<String>,
}

impl ChoiceLabel {
    /// The row's details joined into one line, as a two-row layout shows them.
    pub fn details_text(&self) -> String {
        self.details.join(DETAIL_SEGMENT_SEPARATOR)
    }

    /// The whole row on one line: identity, then each detail segment.
    pub fn single_line(&self) -> String {
        if self.details.is_empty() {
            return self.identity.clone();
        }
        format!(
            "{}{DETAIL_SEGMENT_SEPARATOR}{}",
            self.identity,
            self.details_text()
        )
    }
}

/// Parts of the `/models` row for a plain provider/model catalog entry.
pub(crate) fn model_choice_parts(option: &ProviderModelOption) -> ChoiceLabel {
    format_model_choice_label(
        format!("{}/{}", option.provider_connection, option.model.id),
        option,
    )
}

/// Parts of the `/models` row for a named-engine shortcut, whose identity is
/// prefixed with the engine name so it reads as a shortcut to the
/// provider/model it resolves to.
pub(crate) fn engine_choice_parts(engine_name: &str, option: &ProviderModelOption) -> ChoiceLabel {
    format_model_choice_label(
        format!(
            "{engine_name} → {}/{}",
            option.provider_connection, option.model.id
        ),
        option,
    )
}

fn format_model_choice_label(identity: String, option: &ProviderModelOption) -> ChoiceLabel {
    // The provider-advertised default effort, rendered only in the picker's
    // default slot as `default (X)`.
    let provider_default = option
        .model
        .reasoning_default
        .as_deref()
        .map(|reasoning_default| format!(" ({reasoning_default})"))
        .unwrap_or_default();
    let mut details = Vec::new();
    if let Some(context_length) = option.model.context_length {
        details.push(format!(
            "context: {}",
            format_context_length(context_length)
        ));
    }
    details.push(match option.model.reasoning_efforts.as_deref() {
        None => "reasoning: unavailable".to_string(),
        Some([]) => format!("reasoning: default{provider_default}"),
        Some(_) => match option.reasoning_effort.as_deref() {
            Some(reasoning_effort) => format!("reasoning: {reasoning_effort} ⚙"),
            None => format!("reasoning: default{provider_default} ⚙"),
        },
    });
    if let Some(modality_markers) =
        format_input_modality_markers(option.model.input_modalities.as_deref())
    {
        details.push(modality_markers);
    }
    ChoiceLabel { identity, details }
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

/// How each `/models` entry occupies rows in the picker popup. Chosen from the
/// terminal width at draw time, the same way the footer picks its layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PickerRowLayout {
    /// One row per entry: identity and details on a single line.
    #[default]
    SingleRow,
    /// An identity row followed by a details row per entry, for narrow
    /// terminals where a single line would be clipped.
    TwoRows,
}

impl PickerRowLayout {
    /// Number of popup rows one model entry occupies.
    pub(crate) fn rows_per_entry(self) -> usize {
        match self {
            PickerRowLayout::SingleRow => 1,
            PickerRowLayout::TwoRows => 2,
        }
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
    SessionPicker(super::SessionPicker),
    /// `/models` returned; the user picks the model to switch to. `choices`
    /// always holds the full fetched catalog — narrowing by `filter` only
    /// changes which indices are *displayed/selectable*, so an in-progress
    /// reasoning-effort edit on a choice survives the user typing more of
    /// the filter afterwards.
    ModelPicker {
        choices: Vec<ModelChoice>,
        filter: String,
        selected: usize,
        /// Follow-up parameter popup open for the pre-selected model. While
        /// set, the list is not navigable and the popup owns the keys.
        parameters: Option<ModelParameterEdit>,
    },
}

impl SlashOverlay {
    /// Whether the overlay currently captures navigation keys.
    pub fn is_visible(&self) -> bool {
        !matches!(self, SlashOverlay::Hidden)
    }
}

/// One editable row of the model parameter popup, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParameterField {
    /// Effective context window, in tokens.
    ContextLength,
    /// Reasoning effort, cycled through the model's advertised efforts.
    ReasoningEffort,
}

/// Draft of the follow-up parameter popup. Edits stay here until the popup is
/// confirmed; leaving the popup with `esc` discards them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelParameterEdit {
    /// Index into the picker's `choices` of the model being edited.
    pub choice_index: usize,
    /// Rows shown in the popup. The reasoning row is only present when the
    /// model advertises selectable efforts.
    pub fields: Vec<ParameterField>,
    /// Index into `fields` of the highlighted row.
    pub cursor: usize,
    /// Draft context window in tokens. `None` when no window is known.
    pub context_length: Option<usize>,
    /// Catalog maximum the draft context window may not exceed, when known.
    pub context_length_maximum: Option<usize>,
    /// Whether the context window was changed in this popup.
    pub context_length_edited: bool,
    /// Draft reasoning effort. `None` selects the provider default.
    pub reasoning_effort: Option<String>,
}

/// Smallest context-window step and the unit every step is a multiple of.
pub(crate) const CONTEXT_LENGTH_STEP_UNIT: usize = 32 * 1024;

/// Step size for the context window: one eighth of it, rounded down to a
/// multiple of [`CONTEXT_LENGTH_STEP_UNIT`] and never below that unit.
pub(crate) fn context_length_step(context_length: usize) -> usize {
    (context_length / 8 / CONTEXT_LENGTH_STEP_UNIT).max(1) * CONTEXT_LENGTH_STEP_UNIT
}

/// Move the context window one step up (`delta > 0`) or down (`delta < 0`).
///
/// The window stays on multiples of [`CONTEXT_LENGTH_STEP_UNIT`]: from a
/// multiple the step is added or subtracted; from any other value the first
/// move lands on the next multiple in that direction. The result is never
/// below one step unit and never above `maximum` when the catalog knows the
/// model's window, even when that maximum is not a multiple.
pub(crate) fn adjust_context_length(current: usize, delta: isize, maximum: Option<usize>) -> usize {
    let unit = CONTEXT_LENGTH_STEP_UNIT;
    let step = context_length_step(current);
    let on_multiple = current.is_multiple_of(unit);
    let moved = match (delta >= 0, on_multiple) {
        (true, true) => current + step,
        (true, false) => current.div_ceil(unit) * unit,
        (false, true) => current.saturating_sub(step),
        (false, false) => current / unit * unit,
    };
    let bounded = moved.max(unit);
    match maximum {
        Some(maximum) => bounded.min(maximum),
        None => bounded,
    }
}

/// Cycle the reasoning effort through `None` (provider default) followed by
/// each advertised effort, wrapping at both ends.
pub(crate) fn cycle_reasoning_effort(
    reasoning_efforts: &[String],
    current: Option<&str>,
    delta: isize,
) -> Option<String> {
    let current_index = current
        .and_then(|effort| {
            reasoning_efforts
                .iter()
                .position(|candidate| candidate == effort)
                .map(|index| index + 1)
        })
        .unwrap_or(0);
    let option_count = reasoning_efforts.len() + 1;
    let next_index = (current_index as isize + delta).rem_euclid(option_count as isize) as usize;
    if next_index == 0 {
        None
    } else {
        Some(reasoning_efforts[next_index - 1].clone())
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
/// configured engines and provider models. Under [`PickerRowLayout::TwoRows`]
/// an entry occupies two consecutive rows carrying the same index.
pub(crate) fn model_picker_row_entries(
    choices: &[ModelChoice],
    filter: &str,
    row_layout: PickerRowLayout,
) -> Vec<Option<usize>> {
    let visible_indices = matching_model_indices(choices, filter);
    let separator_before = visible_indices
        .iter()
        .position(|&index| choices[index].engine_name.is_none())
        .filter(|&boundary| boundary > 0);
    let rows_per_entry = row_layout.rows_per_entry();
    (0..visible_indices.len())
        .flat_map(|index| {
            let separator = (separator_before == Some(index)).then_some(None);
            separator
                .into_iter()
                .chain(std::iter::repeat_n(Some(index), rows_per_entry))
        })
        .collect()
}

/// Whether `rows[row_index]` is the details row of a two-row entry, i.e. it
/// repeats the entry of the row directly above it.
pub(crate) fn model_picker_row_is_details(rows: &[Option<usize>], row_index: usize) -> bool {
    row_index > 0 && rows[row_index].is_some() && rows[row_index - 1] == rows[row_index]
}

/// The choices visible under `filter`, in display order.
pub(crate) fn visible_model_choices<'a>(
    choices: &'a [ModelChoice],
    filter: &str,
) -> Vec<&'a ModelChoice> {
    matching_model_indices(choices, filter)
        .into_iter()
        .map(|choice_index| &choices[choice_index])
        .collect()
}

/// Pick the row layout for the visible `/models` choices: one row per model
/// when every single-line label fits in `single_row_text_width` columns,
/// otherwise two rows for every model.
pub(crate) fn model_picker_row_layout(
    choices: &[ModelChoice],
    filter: &str,
    single_row_text_width: usize,
) -> PickerRowLayout {
    let every_label_fits = visible_model_choices(choices, filter)
        .iter()
        .all(|choice| choice.label.single_line().chars().count() <= single_row_text_width);
    if every_label_fits {
        PickerRowLayout::SingleRow
    } else {
        PickerRowLayout::TwoRows
    }
}

/// Text of each parameter-popup row: the label, then the draft value.
pub(crate) fn model_parameter_rows(edit: &ModelParameterEdit, choice: &ModelChoice) -> Vec<String> {
    edit.fields
        .iter()
        .map(|field| match field {
            ParameterField::ContextLength => format!(
                "Context length:  {}",
                edit.context_length
                    .map_or_else(|| "unknown".to_string(), format_context_length)
            ),
            ParameterField::ReasoningEffort => {
                let provider_default = choice
                    .selection
                    .model
                    .reasoning_default
                    .as_deref()
                    .map(|reasoning_default| format!(" ({reasoning_default})"))
                    .unwrap_or_default();
                let value = edit
                    .reasoning_effort
                    .clone()
                    .unwrap_or_else(|| format!("default{provider_default}"));
                format!("Reasoning effort:  {value}")
            }
        })
        .collect()
}

/// Return the plain text represented by each row in a slash overlay. Under
/// [`PickerRowLayout::TwoRows`] a model entry contributes its identity row and
/// then its details row. Model-picker divider rows are represented by an
/// empty string because they have no selectable text.
pub(crate) fn slash_overlay_text_rows(
    overlay: &SlashOverlay,
    row_layout: PickerRowLayout,
) -> Vec<String> {
    match overlay {
        SlashOverlay::Hidden => Vec::new(),
        SlashOverlay::Suggestions { choices, .. } => choices
            .iter()
            .map(|info| format!("{:<8} {}", info.name, info.description))
            .collect(),
        SlashOverlay::LoadingModels => vec!["Loading models…".to_string()],
        SlashOverlay::SessionPicker(picker) => {
            if picker.choices.is_empty() {
                vec![picker.status()]
            } else {
                picker
                    .choices
                    .iter()
                    .map(|choice| format!("{}  {}", choice.last_used_at, choice.title))
                    .collect()
            }
        }
        SlashOverlay::ModelPicker {
            parameters: Some(edit),
            choices,
            ..
        } => choices
            .get(edit.choice_index)
            .map(|choice| model_parameter_rows(edit, choice))
            .unwrap_or_default(),
        SlashOverlay::ModelPicker {
            choices, filter, ..
        } => {
            let visible_choices = visible_model_choices(choices, filter);
            if visible_choices.is_empty() {
                vec!["Nothing matched your model filter.".to_string()]
            } else {
                let rows = model_picker_row_entries(choices, filter, row_layout);
                rows.iter()
                    .enumerate()
                    .map(|(row_index, entry)| {
                        let Some(choice) =
                            entry.and_then(|visible_index| visible_choices.get(visible_index))
                        else {
                            return String::new();
                        };
                        match row_layout {
                            PickerRowLayout::SingleRow => choice.label.single_line(),
                            PickerRowLayout::TwoRows => {
                                if model_picker_row_is_details(&rows, row_index) {
                                    choice.label.details_text()
                                } else {
                                    choice.label.identity.clone()
                                }
                            }
                        }
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
        if matches!(
            self.slash_overlay,
            SlashOverlay::LoadingModels | SlashOverlay::SessionPicker(_)
        ) {
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
            SlashOverlay::SessionPicker(picker) => {
                if picker.phase == super::SessionPickerPhase::Ready {
                    picker.move_selection(delta);
                }
            }
            SlashOverlay::ModelPicker {
                choices,
                filter,
                selected,
                parameters,
            } => {
                if parameters.is_some() {
                    self.move_model_parameter_cursor(delta);
                    return;
                }
                let visible_count = matching_model_indices(choices, filter).len();
                move_selection(selected, visible_count, delta);
            }
            _ => {}
        }
    }

    /// Open the parameter popup for the highlighted model. Does nothing unless
    /// the picker is open, a model is highlighted and no popup is open yet.
    pub fn open_model_parameters(&mut self) {
        let SlashOverlay::ModelPicker {
            choices,
            filter,
            selected,
            parameters,
        } = &mut self.slash_overlay
        else {
            return;
        };
        if parameters.is_some() {
            return;
        }
        let Some(&choice_index) = matching_model_indices(choices, filter).get(*selected) else {
            return;
        };
        let choice = &choices[choice_index];
        let reasoning_efforts = choice
            .selection
            .model
            .reasoning_efforts
            .as_deref()
            .filter(|efforts| !efforts.is_empty());
        let mut fields = vec![ParameterField::ContextLength];
        if reasoning_efforts.is_some() {
            fields.push(ParameterField::ReasoningEffort);
        }
        *parameters = Some(ModelParameterEdit {
            choice_index,
            fields,
            cursor: 0,
            context_length: choice
                .selection_target
                .as_ref()
                .map(|target| target.context_length)
                .or(choice.selection.model.context_length),
            context_length_maximum: choice.selection.model.context_length,
            context_length_edited: false,
            reasoning_effort: choice.selection.reasoning_effort.clone(),
        });
    }

    /// Move the highlighted parameter row by `delta`, clamped to the rows shown.
    pub fn move_model_parameter_cursor(&mut self, delta: isize) {
        let Some(edit) = self.model_parameters_mut() else {
            return;
        };
        let last_row = edit.fields.len().saturating_sub(1) as isize;
        edit.cursor = (edit.cursor as isize + delta).clamp(0, last_row) as usize;
    }

    /// Adjust the highlighted parameter: step the context window or cycle the
    /// reasoning effort.
    pub fn adjust_model_parameter(&mut self, delta: isize) {
        let (choice_index, field) = match self.model_parameters_mut() {
            Some(edit) => match edit.fields.get(edit.cursor).copied() {
                Some(field) => (edit.choice_index, field),
                None => return,
            },
            None => return,
        };
        let reasoning_efforts = match &self.slash_overlay {
            SlashOverlay::ModelPicker { choices, .. } => choices
                .get(choice_index)
                .and_then(|choice| choice.selection.model.reasoning_efforts.clone()),
            _ => None,
        };
        let Some(edit) = self.model_parameters_mut() else {
            return;
        };
        match field {
            ParameterField::ContextLength => {
                let Some(current) = edit.context_length else {
                    return;
                };
                edit.context_length = Some(adjust_context_length(
                    current,
                    delta,
                    edit.context_length_maximum,
                ));
                edit.context_length_edited = true;
            }
            ParameterField::ReasoningEffort => {
                let Some(reasoning_efforts) = reasoning_efforts else {
                    return;
                };
                edit.reasoning_effort = cycle_reasoning_effort(
                    &reasoning_efforts,
                    edit.reasoning_effort.as_deref(),
                    delta,
                );
            }
        }
    }

    /// Apply the popup draft to the edited model and close the popup. The
    /// caller then switches to the highlighted choice as usual.
    pub fn confirm_model_parameters(&mut self) {
        let SlashOverlay::ModelPicker {
            choices,
            parameters,
            ..
        } = &mut self.slash_overlay
        else {
            return;
        };
        let Some(edit) = parameters.take() else {
            return;
        };
        let Some(choice) = choices.get_mut(edit.choice_index) else {
            return;
        };
        if edit.fields.contains(&ParameterField::ReasoningEffort) {
            choice.selection.reasoning_effort = edit.reasoning_effort;
        }
        if edit.context_length_edited {
            choice.selection.context_length_override = edit.context_length;
            if let (Some(target), Some(context_length)) =
                (choice.selection_target.as_mut(), edit.context_length)
            {
                target.context_length = context_length;
            }
        }
        choice.refresh_label();
    }

    /// Close the parameter popup without applying its draft.
    pub fn leave_model_parameters(&mut self) {
        if let SlashOverlay::ModelPicker { parameters, .. } = &mut self.slash_overlay {
            *parameters = None;
        }
    }

    fn model_parameters_mut(&mut self) -> Option<&mut ModelParameterEdit> {
        match &mut self.slash_overlay {
            SlashOverlay::ModelPicker { parameters, .. } => parameters.as_mut(),
            _ => None,
        }
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
                ..
            } => {
                let actual_index = *matching_model_indices(choices, filter).get(*selected)?;
                choices.get(actual_index)
            }
            _ => None,
        }
    }
}
