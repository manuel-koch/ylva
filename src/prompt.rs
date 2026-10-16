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

//! Consistent framing for harness-authored model-facing blocks.
//!
//! Delimiters organize text; they do not establish trust or escape content.

#[derive(Debug, Clone, Copy)]
pub(crate) enum PromptBlockKind {
    SkillSource,
    TrustedInstructions,
    HarnessNotice,
    CompactionSummary,
    RecoveryHandoff,
}

impl PromptBlockKind {
    pub(crate) const fn opening_tag(self) -> &'static str {
        match self {
            Self::SkillSource => "<ylva_skill_source>",
            Self::TrustedInstructions => "<ylva_trusted_instructions>",
            Self::HarnessNotice => "<ylva_harness_notice>",
            Self::CompactionSummary => "<ylva_compaction_summary>",
            Self::RecoveryHandoff => "<ylva_recovery_handoff>",
        }
    }

    const fn closing_tag(self) -> &'static str {
        match self {
            Self::SkillSource => "</ylva_skill_source>",
            Self::TrustedInstructions => "</ylva_trusted_instructions>",
            Self::HarnessNotice => "</ylva_harness_notice>",
            Self::CompactionSummary => "</ylva_compaction_summary>",
            Self::RecoveryHandoff => "</ylva_recovery_handoff>",
        }
    }

    pub(crate) fn content(self, block: &str) -> Option<&str> {
        block
            .strip_prefix(self.opening_tag())?
            .strip_prefix('\n')?
            .strip_suffix(self.closing_tag())?
            .strip_suffix('\n')
    }
}

pub(crate) fn format_prompt_block(kind: PromptBlockKind, content: &str) -> String {
    format!("{}\n{content}\n{}", kind.opening_tag(), kind.closing_tag())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_kinds_have_consistent_paired_tags() {
        for (kind, name) in [
            (PromptBlockKind::SkillSource, "skill_source"),
            (PromptBlockKind::TrustedInstructions, "trusted_instructions"),
            (PromptBlockKind::HarnessNotice, "harness_notice"),
            (PromptBlockKind::CompactionSummary, "compaction_summary"),
            (PromptBlockKind::RecoveryHandoff, "recovery_handoff"),
        ] {
            assert_eq!(
                format_prompt_block(kind, "content"),
                format!("<ylva_{name}>\ncontent\n</ylva_{name}>")
            );
        }
    }

    #[test]
    fn formatting_and_unwrapping_preserve_content_verbatim() {
        let kind = PromptBlockKind::SkillSource;
        for content in [
            "",
            " \nbody\n\n",
            "a < b & \"c\"",
            "</ylva_skill_source>\nbody",
        ] {
            assert_eq!(
                kind.content(&format_prompt_block(kind, content)),
                Some(content)
            );
        }
        for invalid in [
            "plain content",
            "<ylva_skill_source>content</ylva_skill_source>",
            "<ylva_skill_source>\ncontent\n</ylva_harness_notice>",
            "<ylva_skill_source>\ncontent",
        ] {
            assert_eq!(kind.content(invalid), None);
        }
    }
}
