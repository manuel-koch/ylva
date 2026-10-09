//! Pure state for the root-session picker.

use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionAction {
    #[default]
    Resume,
    Clone,
}

impl SessionAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Resume => "Resume",
            Self::Clone => "Clone",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionChoice {
    pub session_id: Uuid,
    pub title: String,
    pub last_used_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPickerPhase {
    Loading,
    Ready,
    Preparing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPicker {
    pub identity: Uuid,
    pub choices: Vec<SessionChoice>,
    pub selected: usize,
    pub action: SessionAction,
    pub phase: SessionPickerPhase,
    pub error: Option<String>,
}

impl SessionPicker {
    pub fn loading() -> Self {
        Self {
            identity: Uuid::new_v4(),
            choices: Vec::new(),
            selected: 0,
            action: SessionAction::Resume,
            phase: SessionPickerPhase::Loading,
            error: None,
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.choices.len().saturating_sub(1));
    }

    pub fn selected(&self) -> Option<&SessionChoice> {
        self.choices.get(self.selected)
    }

    pub fn status(&self) -> String {
        match self.phase {
            SessionPickerPhase::Loading => "Loading sessions...".into(),
            SessionPickerPhase::Preparing => match self.action {
                SessionAction::Resume => "Resuming...".into(),
                SessionAction::Clone => "Cloning...".into(),
            },
            SessionPickerPhase::Ready => self.error.clone().unwrap_or_else(|| {
                if self.choices.is_empty() {
                    "No other root sessions.".into()
                } else {
                    String::new()
                }
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_saturates_and_empty_picker_has_no_selection() {
        let mut picker = SessionPicker::loading();
        picker.move_selection(isize::MAX);
        assert_eq!(picker.selected, 0);
        assert!(picker.selected().is_none());
        picker.phase = SessionPickerPhase::Ready;
        assert_eq!(picker.status(), "No other root sessions.");
        picker.choices = (0..3)
            .map(|index| SessionChoice {
                session_id: Uuid::from_u128(index),
                title: "Untitled".into(),
                last_used_at: "date".into(),
            })
            .collect();
        picker.move_selection(isize::MAX);
        assert_eq!(picker.selected, 2);
        picker.move_selection(isize::MIN);
        assert_eq!(picker.selected, 0);
        assert!(picker.status().is_empty());
    }

    #[test]
    fn status_tracks_operation_and_retains_failures() {
        let mut picker = SessionPicker::loading();
        assert_eq!(picker.status(), "Loading sessions...");
        picker.phase = SessionPickerPhase::Preparing;
        assert_eq!(picker.status(), "Resuming...");
        picker.action = SessionAction::Clone;
        assert_eq!(picker.status(), "Cloning...");
        picker.phase = SessionPickerPhase::Ready;
        picker.error = Some("Clone failed".into());
        assert_eq!(picker.status(), "Clone failed");
        assert_ne!(picker.identity, SessionPicker::loading().identity);
        assert_eq!(SessionPicker::loading().action, SessionAction::Resume);
    }
}
