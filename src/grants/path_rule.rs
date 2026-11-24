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

//! Path matchers for `read`, `write`, `edit`, `search`, `glob` and the
//! `load_skill` source.
//!
//! A rule is either workspace-relative (`./src/*.rs`, resolved against the
//! workspace root and matching only while the resolved target stays inside
//! it) or absolute. Matching reuses the sandbox's custom-glob semantics
//! (component-aware wildcards, dot-component rules). Rules are literal: `~`
//! and `$NAME` are never expanded.

use std::path::Path;

use glob::Pattern;

use super::GrantError;
use crate::sandbox::{expand_path_tokens, path_glob_matches};

/// A validated path rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathRule {
    text: String,
    scope: PathRuleScope,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PathRuleScope {
    /// The rule text after `./`.
    Workspace(String),
    Absolute,
}

impl PathRule {
    /// Validate a stored or user-edited rule.
    pub fn parse(text: &str) -> Result<PathRule, GrantError> {
        if text.is_empty() {
            return Err(GrantError::new("the path rule must not be empty"));
        }
        if text.contains('\n') || text.contains('\r') {
            return Err(GrantError::new("the path rule must be a single line"));
        }
        if text == "**" {
            return Err(GrantError::new(
                "a bare `**` rule is not allowed; use `**/*` to match everything below a directory",
            ));
        }
        let expands = |name: &str| Some(name.to_string());
        if text.starts_with('~') || expand_path_tokens(text, &expands) != text {
            return Err(GrantError::new(
                "the path rule is literal: `~` and `$NAME` are not expanded; write the path out",
            ));
        }
        if text.split('/').any(|component| component == "..") {
            return Err(GrantError::new(
                "the path rule must not contain `..` components",
            ));
        }
        // `.` and `./` name the workspace root itself.
        let workspace_remainder = if text == "." {
            Some("")
        } else {
            text.strip_prefix("./")
        };
        let scope = if let Some(remainder) = workspace_remainder {
            Pattern::new(remainder)
                .map_err(|error| GrantError::new(format!("invalid path glob: {error}")))?;
            PathRuleScope::Workspace(remainder.to_string())
        } else if Path::new(text).is_absolute() {
            Pattern::new(text)
                .map_err(|error| GrantError::new(format!("invalid path glob: {error}")))?;
            PathRuleScope::Absolute
        } else {
            return Err(GrantError::new(
                "the path rule must start with `./` (workspace-relative) or be an absolute path",
            ));
        };
        Ok(PathRule {
            text: text.to_string(),
            scope,
        })
    }

    /// The exact rule for a resolved path: workspace-relative inside the
    /// workspace, absolute outside, glob metacharacters escaped.
    pub fn exact(resolved: &Path, workspace_root: &Path) -> PathRule {
        let text = match resolved.strip_prefix(workspace_root) {
            Ok(relative) if relative.as_os_str().is_empty() => "./".to_string(),
            Ok(relative) => format!(
                "./{}",
                Pattern::escape(&relative.to_string_lossy().replace('\\', "/"))
            ),
            Err(_) => Pattern::escape(&resolved.to_string_lossy()),
        };
        PathRule::parse(&text).expect("an escaped exact path is always a valid rule")
    }

    /// The rule granting everything below the directory of a rule's target.
    pub fn widened_to_parent_directory(resolved: &Path, workspace_root: &Path) -> Option<PathRule> {
        let parent = resolved.parent()?;
        let directory = PathRule::exact(parent, workspace_root);
        let text = if directory.text.ends_with('/') {
            format!("{}**/*", directory.text)
        } else {
            format!("{}/**/*", directory.text)
        };
        PathRule::parse(&text).ok()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether the rule is anchored at the workspace root.
    pub fn is_workspace_relative(&self) -> bool {
        matches!(self.scope, PathRuleScope::Workspace(_))
    }

    /// The absolute glob this rule stands for given `workspace_root`.
    pub fn resolved_pattern(&self, workspace_root: &Path) -> String {
        match &self.scope {
            PathRuleScope::Absolute => self.text.clone(),
            PathRuleScope::Workspace(remainder) => {
                let root = Pattern::escape(&workspace_root.to_string_lossy());
                if remainder.is_empty() {
                    root
                } else {
                    format!("{}/{remainder}", root.trim_end_matches('/'))
                }
            }
        }
    }

    /// Whether `resolved` (absolute, canonical as far as it exists) is
    /// covered. Workspace-relative rules match only while the target stays
    /// inside `workspace_root`.
    pub fn matches(&self, resolved: &Path, workspace_root: &Path) -> bool {
        if self.is_workspace_relative() && !resolved.starts_with(workspace_root) {
            return false;
        }
        let Ok(pattern) = Pattern::new(&self.resolved_pattern(workspace_root)) else {
            return false;
        };
        path_glob_matches(&pattern, &resolved.to_string_lossy())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn root() -> PathBuf {
        PathBuf::from("/work/space")
    }

    fn matches(rule: &str, target: &str) -> bool {
        PathRule::parse(rule)
            .unwrap()
            .matches(Path::new(target), &root())
    }

    #[test]
    fn exact_file_rule_matches_only_that_file() {
        assert!(matches("./src/main.rs", "/work/space/src/main.rs"));
        assert!(!matches("./src/main.rs", "/work/space/src/lib.rs"));
        assert!(!matches("./src/main.rs", "/work/space/src"));
    }

    #[test]
    fn directory_rule_does_not_cover_files_inside() {
        assert!(matches("./src", "/work/space/src"));
        assert!(!matches("./src", "/work/space/src/main.rs"));
    }

    #[test]
    fn single_star_stays_in_one_component_and_double_star_crosses() {
        assert!(matches("./src/*", "/work/space/src/a.rs"));
        assert!(!matches("./src/*", "/work/space/src/sub/a.rs"));
        assert!(matches("./src/**/*", "/work/space/src/sub/deep/a.rs"));
        assert!(matches("./src/**/*", "/work/space/src/a.rs"));
    }

    #[test]
    fn wildcards_do_not_match_hidden_components_unless_spelled() {
        assert!(!matches("./*", "/work/space/.env"));
        assert!(!matches("./**/*", "/work/space/.git/config"));
        assert!(matches("./.env", "/work/space/.env"));
        assert!(matches("./.git/*", "/work/space/.git/config"));
    }

    #[test]
    fn relative_rules_never_match_outside_the_workspace() {
        assert!(!matches("./**/*", "/elsewhere/file"));
        assert!(!matches("./a", "/work/space-other/a"));
    }

    #[test]
    fn absolute_rules_match_anywhere() {
        assert!(matches("/etc/hosts", "/etc/hosts"));
        assert!(!matches("/etc/hosts", "/etc/passwd"));
        assert!(matches("/opt/**/*.txt", "/opt/a/b/c.txt"));
    }

    #[test]
    fn workspace_root_rule_matches_only_the_root() {
        assert!(matches("./", "/work/space"));
        assert!(matches(".", "/work/space"));
        assert!(!matches("./", "/work/space/a"));
    }

    #[test]
    fn root_with_glob_characters_is_matched_literally() {
        let weird_root = PathBuf::from("/work/[sp]*ce");
        let rule = PathRule::parse("./a").unwrap();
        assert!(rule.matches(Path::new("/work/[sp]*ce/a"), &weird_root));
        assert!(!rule.matches(Path::new("/work/space/a"), &weird_root));
    }

    #[test]
    fn invalid_rules_are_rejected_with_an_explanation() {
        for rule in [
            "",
            "**",
            "src/main.rs",
            "~/notes",
            "$HOME/notes",
            "${HOME}/notes",
            "./a/../b",
            "./a\nb",
            "./[",
        ] {
            assert!(PathRule::parse(rule).is_err(), "{rule:?} must be rejected");
        }
        assert!(
            PathRule::parse("**/*").is_err(),
            "relative without ./ is rejected"
        );
        assert!(PathRule::parse("/**/*").is_ok());
        assert!(PathRule::parse("./**/*").is_ok());
    }

    #[test]
    fn exact_rule_escapes_metacharacters_and_roundtrips() {
        let target = Path::new("/work/space/dir/[x]*?.rs");
        let rule = PathRule::exact(target, &root());
        assert_eq!(rule.text(), "./dir/[[]x[]][*][?].rs");
        assert!(rule.matches(target, &root()));
        assert!(!rule.matches(Path::new("/work/space/dir/xa.rs"), &root()));
    }

    #[test]
    fn exact_rule_outside_the_workspace_is_absolute() {
        let rule = PathRule::exact(Path::new("/etc/hosts"), &root());
        assert_eq!(rule.text(), "/etc/hosts");
        assert!(!rule.is_workspace_relative());
    }

    #[test]
    fn exact_rule_of_the_workspace_root_is_dot_slash() {
        assert_eq!(PathRule::exact(&root(), &root()).text(), "./");
    }

    #[test]
    fn widening_covers_the_parent_directory_tree() {
        let widened =
            PathRule::widened_to_parent_directory(Path::new("/work/space/src/a.rs"), &root())
                .unwrap();
        assert_eq!(widened.text(), "./src/**/*");
        let from_root =
            PathRule::widened_to_parent_directory(Path::new("/work/space/a.rs"), &root()).unwrap();
        assert_eq!(from_root.text(), "./**/*");
    }

    #[test]
    fn moved_workspace_keeps_relative_rules_valid() {
        let rule = PathRule::parse("./src/a.rs").unwrap();
        assert!(rule.matches(Path::new("/new/place/src/a.rs"), Path::new("/new/place")));
        assert!(!rule.matches(Path::new("/work/space/src/a.rs"), Path::new("/new/place")));
    }

    #[test]
    fn symlink_escape_is_judged_on_the_resolved_target() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = std::fs::canonicalize(directory.path()).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_file = std::fs::canonicalize(outside.path())
            .unwrap()
            .join("secret");
        std::fs::write(&outside_file, "x").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside_file, workspace.join("link")).unwrap();
            let resolved =
                crate::sandbox::SandboxContext::canonicalize_best_effort(&workspace.join("link"));
            let rule = PathRule::parse("./link").unwrap();
            assert!(!rule.matches(&resolved, &workspace));
            assert!(PathRule::parse("./**/*")
                .unwrap()
                .matches(&workspace.join("a"), &workspace));
            assert!(!PathRule::parse("./**/*")
                .unwrap()
                .matches(&resolved, &workspace));
        }
    }
}
