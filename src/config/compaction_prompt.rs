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

//! Resolution of the compaction summary prompt (`COMPACTION.md`).

use std::path::Path;

use crate::ylva_home::ylva_home;

use super::layered::resolve_file;
use super::ConfigError;

/// Built-in summary instruction used when no workspace or Ylva home
/// `COMPACTION.md` exists.
///
/// It frames the immutable prefix snapshot handed to the compact engine. The
/// summarizer has no tools and never sees the verbatim tail it is cut
/// against.
pub const DEFAULT_COMPACTION_PROMPT: &str = r#"
Condense the supplied conversation into a concise, self-contained summary
that preserves the context needed to continue the work.

Preserve:
- the user's goals, requirements, constraints, and permissions
- decisions and their rationale, including changes to earlier decisions
- completed work, current progress, open tasks, blockers, and next steps
- relevant findings, tool results, errors, and verification outcomes
- exact file paths, URLs, citations, and identifiers needed to continue
- unresolved questions and uncertainty stated in the conversation

Put the current state first. Merge repeated information. Reduce earlier
discussion to the decisions and outcomes that still matter. Keep details
that affect the next action.

Base every statement on the source material. Make clear which actions were
proposed, attempted, or completed, and which findings were confirmed.

Return only the summary text. Use short sections or bullets where useful."#;

/// Load the compaction summary prompt from workspace, Ylva home, or the
/// built-in floor.
///
/// Layering mirrors [`super::soul::load_soul`]: `.ylva/COMPACTION.md` in the
/// workspace wins, then `~/.ylva/COMPACTION.md`, then
/// [`DEFAULT_COMPACTION_PROMPT`].
pub fn load_compaction_prompt(workspace_root: Option<&Path>) -> Result<String, ConfigError> {
    let home = ylva_home();
    load_compaction_prompt_from_layers(workspace_root, home.as_deref())
}

fn load_compaction_prompt_from_layers(
    workspace_root: Option<&Path>,
    ylva_home: Option<&Path>,
) -> Result<String, ConfigError> {
    let relative_path = Path::new("COMPACTION.md");
    let resolved = resolve_file(
        relative_path,
        workspace_root,
        ylva_home,
        Some(DEFAULT_COMPACTION_PROMPT),
    )?
    .expect("the built-in compaction prompt must always resolve");
    Ok(resolved.contents.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_in_prompt_uses_positive_compaction_guidelines() {
        let words: Vec<_> = DEFAULT_COMPACTION_PROMPT
            .split(|character: char| !(character.is_alphabetic() || character == '\''))
            .filter(|word| !word.is_empty())
            .map(str::to_ascii_lowercase)
            .collect();
        for prohibited_word in ["not", "don't", "never", "avoid"] {
            assert!(
                !words.iter().any(|word| word == prohibited_word),
                "the prompt must use positive guidelines instead of {prohibited_word}"
            );
        }
        assert!(words.iter().any(|word| word == "condense"));
        assert!(words.iter().any(|word| word == "concise"));
        assert!(!DEFAULT_COMPACTION_PROMPT.contains("tail begins"));
    }

    #[test]
    fn the_built_in_prompt_is_available_without_disk_configuration() {
        let prompt = load_compaction_prompt_from_layers(None, None).expect("built-in prompt loads");
        assert!(
            prompt.starts_with(DEFAULT_COMPACTION_PROMPT),
            "the built-in prompt retains its default opening line"
        );
        assert!(!prompt.ends_with(char::is_whitespace));
    }

    #[test]
    fn workspace_prompt_wins_over_home_and_built_in_prompts() {
        let temporary_directory = tempfile::tempdir().expect("tempdir");
        let workspace_root = temporary_directory.path().join("workspace");
        let ylva_home = temporary_directory.path().join("home");
        std::fs::create_dir_all(workspace_root.join(".ylva")).expect("workspace .ylva");
        std::fs::create_dir_all(&ylva_home).expect("Ylva home");
        std::fs::write(
            workspace_root.join(".ylva/COMPACTION.md"),
            "Summarize tersely.\n\n",
        )
        .expect("workspace prompt");
        std::fs::write(ylva_home.join("COMPACTION.md"), "home prompt").expect("home prompt");

        assert_eq!(
            load_compaction_prompt_from_layers(Some(&workspace_root), Some(&ylva_home))
                .expect("workspace prompt loads"),
            "Summarize tersely."
        );
    }

    #[test]
    fn home_prompt_wins_over_the_built_in_prompt() {
        let temporary_directory = tempfile::tempdir().expect("tempdir");
        let ylva_home = temporary_directory.path().join("home");
        std::fs::create_dir_all(&ylva_home).expect("Ylva home");
        std::fs::write(ylva_home.join("COMPACTION.md"), "home prompt\n").expect("home prompt");

        assert_eq!(
            load_compaction_prompt_from_layers(None, Some(&ylva_home)).expect("home prompt loads"),
            "home prompt"
        );
    }
}
