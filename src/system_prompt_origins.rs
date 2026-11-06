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

//! The system prompt as an ordered list of sections, each tagged with where
//! its text came from.
//!
//! The prompt string sent to the model is derived from these sections
//! ([`SystemPromptOrigins::render`]), so the origins shown by `/status` can
//! never drift from the prompt itself.

/// Separator between consecutive prompt sections.
const SECTION_SEPARATOR: &str = "\n\n";

/// Where one section of the system prompt came from. Sources are sentinel
/// labels (`$YLVA_HOME/SOUL.md`, `sub/AGENTS.md`), never absolute paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptOrigin {
    /// The resolved global `SOUL.md` layer.
    Soul { source: String },
    /// The fixed preamble that frames trusted instruction files.
    InstructionSafetyPreamble,
    /// One trusted instruction file block.
    InstructionFile { source: String },
    /// The agent type's persona text.
    AgentPersona,
    /// The advertisement of callable skills.
    SkillAdvertisement,
    /// Prompt text supplied as a plain string without origin information.
    Unattributed,
}

/// One tagged section of the system prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptSection {
    pub origin: PromptOrigin,
    pub text: String,
}

/// The ordered, origin-tagged sections that make up a system prompt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemPromptOrigins {
    sections: Vec<PromptSection>,
}

impl SystemPromptOrigins {
    /// A prompt that is a single section of unknown origin.
    pub fn unattributed(text: impl Into<String>) -> Self {
        let mut origins = Self::default();
        origins.push(PromptOrigin::Unattributed, text);
        origins
    }

    pub fn push(&mut self, origin: PromptOrigin, text: impl Into<String>) {
        self.sections.push(PromptSection {
            origin,
            text: text.into(),
        });
    }

    pub fn is_empty(&self) -> bool {
        self.sections.is_empty()
    }

    pub fn sections(&self) -> &[PromptSection] {
        &self.sections
    }

    /// The prompt text: every section joined by a blank line.
    pub fn render(&self) -> String {
        self.sections
            .iter()
            .map(|section| section.text.as_str())
            .collect::<Vec<_>>()
            .join(SECTION_SEPARATOR)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_joins_sections_with_a_blank_line_in_order() {
        let mut origins = SystemPromptOrigins::default();
        origins.push(
            PromptOrigin::Soul {
                source: "built-in".into(),
            },
            "soul",
        );
        origins.push(PromptOrigin::AgentPersona, "persona");

        assert_eq!(origins.render(), "soul\n\npersona");
        assert_eq!(origins.sections().len(), 2);
    }

    #[test]
    fn an_unattributed_prompt_renders_unchanged() {
        let origins = SystemPromptOrigins::unattributed("plain prompt");

        assert_eq!(origins.render(), "plain prompt");
        assert_eq!(origins.sections()[0].origin, PromptOrigin::Unattributed);
        assert!(!origins.is_empty());
    }
}
