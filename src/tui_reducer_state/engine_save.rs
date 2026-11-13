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

//! Save form of the model picker's parameters page: persist the draft as a
//! named engine (create or update) in the startup-loaded profile file.
//!
//! The form edits nothing but its own state. Submitting validates the draft
//! and emits [`Effect::SaveEngine`]; the driver's save owner performs the
//! write and reports back through [`AppState::finish_engine_save`].

use std::collections::{HashMap, HashSet};

use super::model_picker::{ModelParameterEdit, ParameterField, SlashOverlay};
use super::{AppState, Effect};
use crate::config::{
    validate_engine_name, EngineSaveChange, EngineSaveMode, EngineSaveRequest, DEFAULT_PROFILE_NAME,
};
use crate::llm::engine::{EngineConfig, ProviderModelOption};

/// Startup-sourced facts the save form validates against.
#[derive(Debug, Clone, Default)]
pub struct EngineSaveContext {
    /// Setup guidance when engines cannot be saved at all (no profile file).
    pub unavailable: Option<String>,
    /// Display path of the profile file saves are written to.
    pub destination: String,
    /// Whether the destination is a workspace profile that needs re-trusting.
    pub workspace_layer: bool,
    /// Provider connections that have a `[provider_connections.<name>]` table.
    pub persistable_connections: HashSet<String>,
    /// Engines defined by an `[engines.<name>]` table, as configured.
    pub explicit_engines: HashMap<String, EngineConfig>,
    /// Every engine name in use at runtime, explicit or implicit.
    pub occupied_names: HashSet<String>,
}

impl EngineSaveContext {
    /// Why `name` cannot be used for a new engine, if it cannot.
    pub fn name_problem(&self, name: &str) -> Option<String> {
        if let Err(problem) = validate_engine_name(name) {
            return Some(problem);
        }
        if name == DEFAULT_PROFILE_NAME {
            // The unset `default_engine` resolves to this name, so a new
            // engine of that name would silently take over every agent.
            return Some(format!(
                "the engine name '{name}' is reserved: it is what `default_engine` resolves to when unset"
            ));
        }
        self.occupied_names
            .contains(name)
            .then(|| format!("the engine name '{name}' is already in use"))
    }
}

/// One engine save, as handed to the driver's save owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineSaveJob {
    pub request: EngineSaveRequest,
    /// Activate the saved engine once it is persisted and published.
    pub activate: bool,
    /// Selection the saved engine is activated with.
    pub activation_option: ProviderModelOption,
}

/// Focusable controls of the save form, in tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineSaveFocus {
    /// Create/Update mode switch; a stop only when updating is possible.
    Mode,
    Name,
    Save,
    SaveAndApply,
}

impl EngineSaveFocus {
    pub fn is_button(self) -> bool {
        matches!(self, Self::Save | Self::SaveAndApply)
    }
}

/// Caret movement inside the name field of the save page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameCursorMove {
    Left,
    Right,
    Start,
    End,
}

/// State of the save page while it is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineSaveForm {
    /// Update the origin engine instead of creating a new one.
    pub update: bool,
    /// Whether the origin is an explicit engine that can be updated.
    pub update_available: bool,
    /// Name draft for a new engine.
    pub name: String,
    /// Caret position within `name`, in characters.
    pub name_cursor: usize,
    pub focus: EngineSaveFocus,
    /// Validation or write failure of the last submission.
    pub error: Option<String>,
    /// A submitted save has not completed yet; further submissions are blocked.
    pub pending: bool,
    /// Name of the explicit engine the draft came from, when there is one.
    pub origin_name: Option<String>,
    /// What saving would write, refreshed after every edit of the form.
    pub preview: Option<EngineSavePreview>,
    /// Why the name draft of a new engine cannot be saved; the save buttons
    /// are disabled while this is set.
    pub name_problem: Option<String>,
}

const NAME_ROW_PREFIX: &str = "Name:  ";

/// Number of fixed button rows at the end of [`EngineSaveForm::entries`].
pub const ENGINE_SAVE_BUTTON_COUNT: usize = 2;

impl EngineSaveForm {
    /// Character column of the caret within the name row text, while the
    /// name field is focused and editable.
    pub fn name_caret_column(&self) -> Option<usize> {
        (self.focus == EngineSaveFocus::Name && !self.update)
            .then(|| NAME_ROW_PREFIX.chars().count() + self.name_cursor)
    }

    fn name_byte_index(&self) -> usize {
        self.name
            .char_indices()
            .nth(self.name_cursor)
            .map_or(self.name.len(), |(index, _)| index)
    }

    /// Whether the save buttons can be used.
    pub fn buttons_enabled(&self) -> bool {
        !self.pending && (self.update || self.name_problem.is_none())
    }

    /// Text of every row of the save page. The last
    /// [`ENGINE_SAVE_BUTTON_COUNT`] rows are the buttons.
    pub fn entries(&self) -> Vec<String> {
        let mut entries = Vec::new();
        entries.push(match (&self.origin_name, self.update) {
            (Some(origin), true) => {
                format!("Mode:  [ ] Create new engine   [X] Update engine '{origin}'")
            }
            (Some(origin), false) => {
                format!("Mode:  [X] Create new engine   [ ] Update engine '{origin}'")
            }
            (None, _) => "Mode:  Create new engine".to_string(),
        });
        entries.push(match (&self.origin_name, self.update) {
            (Some(origin), true) => format!("Name:  {origin}"),
            _ => {
                format!("{NAME_ROW_PREFIX}{}", self.name)
            }
        });
        entries.push("Save".to_string());
        entries.push("Save & apply".to_string());
        entries
    }

    /// Non-editable text of the save page — destination, what saving writes,
    /// trust guidance and the last failure — shown in the page's info area
    /// below the controls.
    pub fn info_lines(&self) -> Vec<String> {
        // Problems come first so they are never scrolled out of view.
        let mut lines = Vec::new();
        if let Some(problem) = self.name_problem.as_ref().filter(|_| !self.update) {
            lines.push(format!("✗ {problem}"));
        }
        if let Some(error) = &self.error {
            lines.push(format!("✗ {error}"));
        }
        if let Some(preview) = &self.preview {
            lines.push(format!("Destination:  {}", preview.destination));
            lines.push("Model parameters:".to_string());
            lines.extend(preview.lines.iter().map(|line| format!("  {line}")));
            if let Some(guidance) = &preview.trust_guidance {
                lines.push(guidance.clone());
            }
        }
        if self.pending {
            lines.push("Saving…".to_string());
        }
        lines
    }

    /// Entry index highlighted for the focus, within `entries_len` entries.
    pub fn focus_entry(&self, entries_len: usize) -> usize {
        match self.focus {
            EngineSaveFocus::Mode => 0,
            EngineSaveFocus::Name => 1,
            EngineSaveFocus::Save => entries_len - ENGINE_SAVE_BUTTON_COUNT,
            EngineSaveFocus::SaveAndApply => entries_len - 1,
        }
    }

    /// Control behind an entry, for pointer hit testing.
    pub fn entry_focus(&self, entry_index: usize, entries_len: usize) -> Option<EngineSaveFocus> {
        let first_button = entries_len - ENGINE_SAVE_BUTTON_COUNT;
        match entry_index {
            0 if self.update_available => Some(EngineSaveFocus::Mode),
            1 if !self.update => Some(EngineSaveFocus::Name),
            index if index == first_button => Some(EngineSaveFocus::Save),
            index if index == first_button + 1 => Some(EngineSaveFocus::SaveAndApply),
            _ => None,
        }
    }

    fn focus_stops(&self) -> Vec<EngineSaveFocus> {
        let mut stops = Vec::new();
        if self.update_available {
            stops.push(EngineSaveFocus::Mode);
        }
        if !self.update {
            stops.push(EngineSaveFocus::Name);
        }
        if self.buttons_enabled() || self.pending {
            stops.push(EngineSaveFocus::Save);
            stops.push(EngineSaveFocus::SaveAndApply);
        }
        stops
    }
}

fn change<T: PartialEq>(draft: Option<T>, configured: Option<T>) -> EngineSaveChange<T> {
    if draft == configured {
        EngineSaveChange::Keep
    } else {
        match draft {
            Some(value) => EngineSaveChange::Set(value),
            None => EngineSaveChange::Remove,
        }
    }
}

/// Name suggestion for a new engine: the model id restricted to the name grammar.
fn suggested_engine_name(model_id: &str, context: &EngineSaveContext) -> String {
    let mut name: String = model_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                character
            } else {
                '-'
            }
        })
        .skip_while(|character| !character.is_ascii_alphanumeric())
        .take(60)
        .collect();
    if name.is_empty() || context.name_problem(&name).is_some() {
        name.push_str("-custom");
    }
    if context.name_problem(&name).is_none() {
        name
    } else {
        String::new()
    }
}

/// Everything the save page shows besides the form controls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineSavePreview {
    pub lines: Vec<String>,
    pub destination: String,
    pub trust_guidance: Option<String>,
}

impl AppState {
    pub fn engine_save_form(&self) -> Option<&EngineSaveForm> {
        match &self.slash_overlay {
            SlashOverlay::ModelPicker {
                parameters: Some(edit),
                ..
            } => edit.save_form.as_deref(),
            _ => None,
        }
    }

    fn engine_save_form_mut(&mut self) -> Option<&mut EngineSaveForm> {
        self.model_parameters_mut()
            .and_then(|edit| edit.save_form.as_deref_mut())
    }

    /// Open the save page for the parameters draft. Does nothing unless the
    /// parameters page is open.
    pub fn open_engine_save_form(&mut self) {
        let context = self.engine_save_context.clone();
        let SlashOverlay::ModelPicker {
            choices,
            parameters: Some(edit),
            ..
        } = &mut self.slash_overlay
        else {
            return;
        };
        let Some(choice) = choices.get(edit.choice_index) else {
            return;
        };
        let origin_is_explicit = choice
            .engine_name
            .as_ref()
            .is_some_and(|name| context.explicit_engines.contains_key(name));
        let suggestion = if origin_is_explicit {
            String::new()
        } else {
            suggested_engine_name(&edit.source_choice(choice).selection.model.id, &context)
        };
        edit.save_form = Some(Box::new(EngineSaveForm {
            update: origin_is_explicit,
            update_available: origin_is_explicit,
            name_cursor: suggestion.chars().count(),
            name: suggestion,
            focus: if origin_is_explicit {
                EngineSaveFocus::Mode
            } else {
                EngineSaveFocus::Name
            },
            error: context.unavailable.clone(),
            pending: false,
            origin_name: choice
                .engine_name
                .clone()
                .filter(|name| context.explicit_engines.contains_key(name)),
            preview: None,
            name_problem: None,
        }));
        self.refresh_engine_save_preview();
    }

    fn refresh_engine_save_preview(&mut self) {
        let preview = self.engine_save_preview();
        let name_problem = self
            .engine_save_form()
            .and_then(|form| self.engine_save_context.name_problem(form.name.trim()));
        if let Some(form) = self.engine_save_form_mut() {
            form.preview = preview;
            form.name_problem = name_problem;
        }
    }

    /// Switch between creating a new engine and updating the origin engine.
    pub fn toggle_engine_save_mode(&mut self) {
        if let Some(form) = self.engine_save_form_mut() {
            if form.update_available && !form.pending {
                form.update = !form.update;
                form.error = None;
            }
        }
        self.refresh_engine_save_preview();
    }

    pub fn engine_save_insert_char(&mut self, character: char) {
        if let Some(form) = self.engine_save_form_mut() {
            if form.focus == EngineSaveFocus::Name && !form.update && !form.pending {
                let byte_index = form.name_byte_index();
                form.name.insert(byte_index, character);
                form.name_cursor += 1;
                form.error = None;
            }
        }
        self.refresh_engine_save_preview();
    }

    /// Delete the character under the caret.
    pub fn engine_save_delete(&mut self) {
        if let Some(form) = self.engine_save_form_mut() {
            if form.focus == EngineSaveFocus::Name
                && !form.update
                && !form.pending
                && form.name_cursor < form.name.chars().count()
            {
                let byte_index = form.name_byte_index();
                form.name.remove(byte_index);
                form.error = None;
            }
        }
        self.refresh_engine_save_preview();
    }

    /// Move the name caret by `delta` characters, or to the start (`None`
    /// with `to_end == false`) / end of the name.
    pub fn engine_save_move_name_cursor(&mut self, target: NameCursorMove) {
        if let Some(form) = self.engine_save_form_mut() {
            if form.focus == EngineSaveFocus::Name && !form.update {
                let length = form.name.chars().count();
                form.name_cursor = match target {
                    NameCursorMove::Left => form.name_cursor.saturating_sub(1),
                    NameCursorMove::Right => (form.name_cursor + 1).min(length),
                    NameCursorMove::Start => 0,
                    NameCursorMove::End => length,
                };
            }
        }
    }

    pub fn engine_save_backspace(&mut self) {
        if let Some(form) = self.engine_save_form_mut() {
            if form.focus == EngineSaveFocus::Name && !form.update && !form.pending {
                if form.name_cursor > 0 {
                    form.name_cursor -= 1;
                    let byte_index = form.name_byte_index();
                    form.name.remove(byte_index);
                }
                form.error = None;
            }
        }
        self.refresh_engine_save_preview();
    }

    pub fn cycle_engine_save_focus(&mut self, backwards: bool) {
        let Some(form) = self.engine_save_form_mut() else {
            return;
        };
        let stops = form.focus_stops();
        let current = stops
            .iter()
            .position(|stop| *stop == form.focus)
            .unwrap_or(0);
        let next = if backwards {
            (current + stops.len() - 1) % stops.len()
        } else {
            (current + 1) % stops.len()
        };
        form.focus = stops[next];
    }

    /// Focus a control directly (mouse click). Ignored when it is not a stop.
    pub fn focus_engine_save_control(&mut self, focus: EngineSaveFocus) {
        if let Some(form) = self.engine_save_form_mut() {
            if form.focus_stops().contains(&focus) {
                form.focus = focus;
            }
        }
    }

    /// Enter: commits only when a button has focus; the mode switch toggles.
    pub fn activate_engine_save_focus(&mut self) -> Vec<Effect> {
        let Some(focus) = self.engine_save_form().map(|form| form.focus) else {
            return Vec::new();
        };
        match focus {
            EngineSaveFocus::Mode => {
                self.toggle_engine_save_mode();
                Vec::new()
            }
            EngineSaveFocus::Name => Vec::new(),
            EngineSaveFocus::Save => self.submit_engine_save(false),
            EngineSaveFocus::SaveAndApply => self.submit_engine_save(true),
        }
    }

    fn submit_engine_save(&mut self, activate: bool) -> Vec<Effect> {
        if self
            .engine_save_form()
            .is_some_and(|form| !form.buttons_enabled())
        {
            return Vec::new();
        }
        match self.build_engine_save_job(activate) {
            Ok(job) => {
                if let Some(form) = self.engine_save_form_mut() {
                    form.pending = true;
                    form.error = None;
                }
                vec![Effect::SaveEngine(Box::new(job))]
            }
            Err(message) => {
                if let Some(form) = self.engine_save_form_mut() {
                    form.error = Some(message);
                }
                Vec::new()
            }
        }
    }

    /// Settle the save page after the owner could not start or finished the
    /// write unsuccessfully: the draft and form stay, with the reason shown.
    pub fn fail_engine_save(&mut self, message: String) {
        if let Some(form) = self.engine_save_form_mut() {
            form.pending = false;
            form.error = Some(message);
        }
    }

    /// Whether the save page is still waiting for the write of a submission,
    /// so a delayed activation may proceed.
    pub fn engine_save_is_pending(&self) -> bool {
        self.engine_save_form().is_some_and(|form| form.pending)
    }

    /// Record a persisted engine in the validation context.
    pub fn record_saved_engine(&mut self, name: &str, config: EngineConfig) {
        self.engine_save_context
            .occupied_names
            .insert(name.to_string());
        self.engine_save_context
            .explicit_engines
            .insert(name.to_string(), config);
    }

    /// Close the whole picker after a confirmed save; the transcript notice
    /// reports the outcome and a reopened picker lists the engine.
    pub fn close_model_picker_after_save(&mut self) {
        if matches!(self.slash_overlay, SlashOverlay::ModelPicker { .. }) {
            self.slash_overlay = SlashOverlay::Hidden;
            self.draft = String::new();
        }
    }

    /// Request and the configured definition it changes, from the draft.
    fn engine_save_request(
        &self,
        form: &EngineSaveForm,
    ) -> Result<(EngineSaveRequest, Option<EngineConfig>, ProviderModelOption), String> {
        let context = &self.engine_save_context;
        let SlashOverlay::ModelPicker {
            choices,
            parameters: Some(edit),
            ..
        } = &self.slash_overlay
        else {
            return Err("the parameters page is not open".to_string());
        };
        let choice = choices
            .get(edit.choice_index)
            .ok_or_else(|| "the edited model is no longer listed".to_string())?;
        let option = edit.source_choice(choice).selection.clone();
        let connection = option.provider_connection.clone();
        if !context.persistable_connections.contains(&connection) {
            return Err(format!(
                "provider connection '{connection}' has no [provider_connections.{connection}] \
                 table in the profile (environment-only or legacy engine); add one to the \
                 profile to save engines for it"
            ));
        }
        let origin_name = choice
            .engine_name
            .clone()
            .filter(|name| context.explicit_engines.contains_key(name));
        let mode = if form.update {
            EngineSaveMode::Update {
                name: origin_name.clone().ok_or_else(|| {
                    "only an explicit [engines.<name>] engine can be updated".to_string()
                })?,
            }
        } else {
            let name = form.name.trim().to_string();
            if let Some(problem) = context.name_problem(&name) {
                return Err(problem);
            }
            EngineSaveMode::Create {
                name,
                copy_from: origin_name.clone(),
            }
        };
        let configured = origin_name
            .as_deref()
            .and_then(|name| context.explicit_engines.get(name))
            .cloned();
        let model = if configured.is_none() {
            EngineSaveChange::Set(option.model.id.clone())
        } else {
            change(
                Some(option.model.id.clone()),
                configured.as_ref().map(|config| config.model.clone()),
            )
        };
        let configured_reasoning = configured
            .as_ref()
            .and_then(|config| config.reasoning.clone());
        let reasoning = if edit.fields.contains(&ParameterField::ReasoningEffort) {
            change(edit.reasoning_effort.clone(), configured_reasoning)
        } else {
            EngineSaveChange::Keep
        };
        let request = EngineSaveRequest {
            mode,
            provider_connection: connection,
            model,
            reasoning,
            context_length: explicit_limit(
                edit.context_length_edited,
                edit.context_length,
                edit.initial_context_length,
                configured.as_ref().and_then(|config| config.context_length),
            ),
            max_output_tokens: explicit_limit(
                edit.max_output_tokens_edited,
                edit.max_output_tokens,
                edit.initial_max_output_tokens,
                configured
                    .as_ref()
                    .and_then(|config| config.max_output_tokens),
            ),
        };
        let mut activation_option = option;
        activation_option.reasoning_effort = None;
        activation_option.context_length_override = None;
        activation_option.max_output_tokens_override = None;
        Ok((request, configured, activation_option))
    }

    fn build_engine_save_job(&self, activate: bool) -> Result<EngineSaveJob, String> {
        if let Some(reason) = &self.engine_save_context.unavailable {
            return Err(reason.clone());
        }
        let form = self
            .engine_save_form()
            .ok_or_else(|| "the save page is not open".to_string())?;
        let (request, _, activation_option) = self.engine_save_request(form)?;
        Ok(EngineSaveJob {
            request,
            activate,
            activation_option,
        })
    }

    /// Preview of the engine definition the current form would save: the full
    /// resulting settings for a new engine, `before → after` for changed
    /// settings of an update. `None` while the draft cannot be previewed.
    pub fn engine_save_preview(&self) -> Option<EngineSavePreview> {
        let form = self.engine_save_form()?;
        let context = &self.engine_save_context;
        let draft_form = EngineSaveForm {
            name: if form.update || form.name.trim().is_empty() {
                // The preview shows settings, not the name, so any valid
                // placeholder keeps request building from failing on naming.
                "preview".to_string()
            } else {
                form.name.clone()
            },
            ..form.clone()
        };
        let (request, configured, _) = self.engine_save_request(&draft_form).ok()?;
        let after = request.preview_lines(configured.as_ref());
        let lines = if form.update {
            let unchanged = EngineSaveRequest {
                mode: request.mode.clone(),
                provider_connection: request.provider_connection.clone(),
                model: EngineSaveChange::Keep,
                reasoning: EngineSaveChange::Keep,
                context_length: EngineSaveChange::Keep,
                max_output_tokens: EngineSaveChange::Keep,
            };
            let before = unchanged.preview_lines(configured.as_ref());
            before
                .into_iter()
                .zip(after)
                .map(|(old, new)| {
                    if old == new {
                        new
                    } else {
                        format!(
                            "{new}  (was: {})",
                            old.split_once(" = ").map_or("", |p| p.1)
                        )
                    }
                })
                .collect()
        } else {
            after
        };
        let trust_guidance = context.workspace_layer.then(|| {
            "The destination is a workspace profile: the next launch of Ylva requires \
             `ylva trust` before it loads this configuration."
                .to_string()
        });
        Some(EngineSavePreview {
            lines,
            destination: context.destination.clone(),
            trust_guidance,
        })
    }
}

/// An output or context limit counts as the user's explicit choice only when
/// it was edited to a value different from the one the popup opened with;
/// otherwise the configured value is kept, so catalog-derived runtime values
/// are never persisted.
fn explicit_limit(
    edited: bool,
    draft: Option<usize>,
    initial: Option<usize>,
    configured: Option<usize>,
) -> EngineSaveChange<usize> {
    let explicit = if edited && draft != initial {
        draft
    } else {
        configured
    };
    change(explicit, configured)
}

impl ModelParameterEdit {
    /// Whether the save page is open on top of the parameters page.
    pub fn save_page_open(&self) -> bool {
        self.save_form.is_some()
    }
}
