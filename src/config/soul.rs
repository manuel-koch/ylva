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

//! Resolution of the global `SOUL.md` persona.

use std::path::Path;

use crate::ylva_home::ylva_home;

use super::layered::{resolve_file, ConfigLayer};
use super::ConfigError;

/// Built-in persona used when no workspace or Ylva home `SOUL.md` exists.
pub const DEFAULT_SOUL: &str = "You are Ylva, a helpful agent with tool capabilities.";

/// The resolved persona text and the layer that supplied it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedSoul {
    pub text: String,
    pub layer: ConfigLayer,
}

impl LoadedSoul {
    /// Sentinel label of the supplying layer; never an absolute path.
    pub fn source(&self) -> &'static str {
        match self.layer {
            ConfigLayer::Workspace => ".ylva/SOUL.md",
            ConfigLayer::Home => "$YLVA_HOME/SOUL.md",
            ConfigLayer::BuiltIn => "built-in",
        }
    }
}

/// Load the global persona from workspace, Ylva home, or the built-in floor.
pub fn load_soul(workspace_root: Option<&Path>) -> Result<String, ConfigError> {
    load_soul_with_layer(workspace_root).map(|soul| soul.text)
}

/// Like [`load_soul`], also reporting which layer supplied the persona.
pub fn load_soul_with_layer(workspace_root: Option<&Path>) -> Result<LoadedSoul, ConfigError> {
    let home = ylva_home();
    load_soul_from_layers(workspace_root, home.as_deref())
}

fn load_soul_from_layers(
    workspace_root: Option<&Path>,
    ylva_home: Option<&Path>,
) -> Result<LoadedSoul, ConfigError> {
    let relative_path = Path::new("SOUL.md");
    let resolved = resolve_file(relative_path, workspace_root, ylva_home, Some(DEFAULT_SOUL))?
        .expect("the built-in SOUL must always resolve");
    Ok(LoadedSoul {
        text: resolved.contents.trim_end().to_string(),
        layer: resolved.layer,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_in_persona_is_available_without_disk_configuration() {
        let persona = load_soul_from_layers(None, None)
            .expect("built-in soul loads")
            .text;
        assert!(
            persona.starts_with(DEFAULT_SOUL),
            "the built-in persona retains the default opening line"
        );
        assert!(!persona.ends_with(char::is_whitespace));
    }

    #[test]
    fn workspace_persona_wins_over_home_and_built_in_personas() {
        let temporary_directory = tempfile::tempdir().expect("tempdir");
        let workspace_root = temporary_directory.path().join("workspace");
        let ylva_home = temporary_directory.path().join("home");
        std::fs::create_dir_all(workspace_root.join(".ylva")).expect("workspace .ylva");
        std::fs::create_dir_all(&ylva_home).expect("Ylva home");
        std::fs::write(workspace_root.join(".ylva/SOUL.md"), "project\n\n")
            .expect("workspace soul");
        std::fs::write(ylva_home.join("SOUL.md"), "home").expect("home soul");

        assert_eq!(
            load_soul_from_layers(Some(&workspace_root), Some(&ylva_home))
                .expect("workspace soul loads"),
            LoadedSoul {
                text: "project".to_string(),
                layer: ConfigLayer::Workspace
            }
        );
    }

    #[test]
    fn each_layer_reports_a_sentinel_source_without_absolute_paths() {
        let source = |layer| {
            LoadedSoul {
                text: String::new(),
                layer,
            }
            .source()
        };

        assert_eq!(source(ConfigLayer::Workspace), ".ylva/SOUL.md");
        assert_eq!(source(ConfigLayer::Home), "$YLVA_HOME/SOUL.md");
        assert_eq!(source(ConfigLayer::BuiltIn), "built-in");
    }

    #[test]
    fn home_persona_wins_over_the_built_in_persona() {
        let temporary_directory = tempfile::tempdir().expect("tempdir");
        let ylva_home = temporary_directory.path().join("home");
        std::fs::create_dir_all(&ylva_home).expect("Ylva home");
        std::fs::write(ylva_home.join("SOUL.md"), "home\n").expect("home soul");

        assert_eq!(
            load_soul_from_layers(None, Some(&ylva_home)).expect("home soul loads"),
            LoadedSoul {
                text: "home".to_string(),
                layer: ConfigLayer::Home
            }
        );
    }
}
