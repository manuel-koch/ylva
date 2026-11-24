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

//! The grants offered for a single pending tool call.

use std::path::Path;

use super::{
    propose_command_rule, CommandRuleScope, CommandStatement, Grant, GrantError, GrantMatcher,
    GrantSubject, GrantSubjects, GrantTool, OriginRule, PathRule,
};

/// The narrowest reusable grants that would cover `subjects` of a `tool`
/// call: an exact path, skill, origin or one proposed rule per simple command
/// that the sandbox policy does not already approve.
///
/// Fails when any part of the call cannot be granted, so a partial grant never
/// suggests that the whole call is covered.
pub fn propose_grants(
    tool: GrantTool,
    subjects: &GrantSubjects,
    workspace_root: &Path,
) -> Result<Vec<Grant>, GrantError> {
    let matcher = match subjects {
        GrantSubjects::NotGrantable => {
            return Err(GrantError::new("this call cannot be granted"));
        }
        GrantSubjects::Single(GrantSubject::Path(path)) => {
            GrantMatcher::Path(PathRule::exact(path, workspace_root))
        }
        GrantSubjects::Single(GrantSubject::Skill { name, source }) => GrantMatcher::Skill {
            name: name.clone(),
            source: PathRule::exact(source, workspace_root),
        },
        GrantSubjects::Single(GrantSubject::Origin(origin)) => {
            GrantMatcher::Origin(OriginRule::parse(origin)?)
        }
        GrantSubjects::Single(GrantSubject::Unconditional) => GrantMatcher::Any,
        GrantSubjects::Single(GrantSubject::Command(_)) => {
            return Err(GrantError::new("this call cannot be granted"));
        }
        GrantSubjects::Commands(statements) => {
            return propose_command_grants(tool, statements);
        }
    };
    Ok(vec![Grant { tool, matcher }])
}

/// A one-line description of `grant` for notices and the grant dialog.
pub fn describe_grant(grant: &Grant) -> String {
    match grant.stored_rule() {
        Some(rule) => format!("{} '{}'", grant.tool.name(), rule),
        None => format!("{} (any call)", grant.tool.name()),
    }
}

/// Consequences of creating `grants` that the user should read before
/// confirming. `marker` is the `.ylva` directory a persistent save would
/// create.
pub fn grant_warnings(grants: &[Grant], marker: Option<&Path>) -> Vec<String> {
    let mut warnings = Vec::new();
    if grants.iter().any(|grant| grant.tool.mutates_files()) {
        warnings.push("The granted tool modifies files without asking again.".to_string());
    }
    if grants.iter().any(|grant| {
        matches!(&grant.matcher, GrantMatcher::Command(rule) if rule.scope() == CommandRuleScope::Prefix)
    }) {
        warnings.push(
            "A trailing * covers one or more further arguments of the command.".to_string(),
        );
    }
    if grants
        .iter()
        .any(|grant| matches!(&grant.matcher, GrantMatcher::Command(rule) if rule.is_wider()))
    {
        warnings.push("A rule covers more than the command you are approving.".to_string());
    }
    if grants.iter().any(|grant| grant.tool == GrantTool::Bash) {
        warnings.push(
            "Environment variables and working directory are not part of a command rule."
                .to_string(),
        );
    }
    if grants
        .iter()
        .any(|grant| matches!(grant.matcher, GrantMatcher::Origin(_)))
    {
        warnings.push("Redirects to other origins are not covered.".to_string());
    }
    if grants
        .iter()
        .any(|grant| matches!(grant.matcher, GrantMatcher::Any))
    {
        warnings.push("Every future call of this tool is covered.".to_string());
    }
    if let Some(marker) = marker {
        warnings.push(format!(
            "Saving a persistent grant creates {}.",
            marker.display()
        ));
    }
    warnings.push(
        "A saved grant changes the workspace trust fingerprint; run `ylva trust` before the \
         next start."
            .to_string(),
    );
    warnings
}

fn propose_command_grants(
    tool: GrantTool,
    statements: &[CommandStatement],
) -> Result<Vec<Grant>, GrantError> {
    let mut grants: Vec<Grant> = Vec::new();
    for statement in statements {
        let command = match statement {
            CommandStatement::Unanalysable => {
                return Err(GrantError::new(
                    "parts of this command can only be approved once",
                ));
            }
            CommandStatement::Command {
                policy_approved: true,
                ..
            } => continue,
            CommandStatement::Command { command, .. } => command,
        };
        let rule = propose_command_rule(command)
            .filter(|rule| rule.matches(command))
            .ok_or_else(|| {
                GrantError::new("a command of this call cannot be granted; approve it once")
            })?;
        let grant = Grant {
            tool,
            matcher: GrantMatcher::Command(rule),
        };
        if !grants.contains(&grant) {
            grants.push(grant);
        }
    }
    if grants.is_empty() {
        return Err(GrantError::new("this call cannot be granted"));
    }
    Ok(grants)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grants::{CommandSubject, CommandToken};
    use std::path::PathBuf;

    fn command(words: &[&str], policy_approved: bool) -> CommandStatement {
        CommandStatement::Command {
            command: CommandSubject {
                tokens: words
                    .iter()
                    .map(|word| CommandToken::new(*word, false))
                    .collect(),
                has_unquoted_expansion: false,
                has_variable_assignment_prefix: false,
            },
            policy_approved,
        }
    }

    #[test]
    fn a_path_call_proposes_the_exact_workspace_relative_path() {
        let root = PathBuf::from("/work");
        let subjects = GrantSubjects::Single(GrantSubject::Path(PathBuf::from("/work/src/a.rs")));
        let grants = propose_grants(GrantTool::Write, &subjects, &root).unwrap();
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].stored_rule().as_deref(), Some("./src/a.rs"));
        assert!(grants[0].covers(&GrantSubject::Path(PathBuf::from("/work/src/a.rs")), &root));
    }

    #[test]
    fn an_unconditional_tool_proposes_an_any_grant() {
        let subjects = GrantSubjects::Single(GrantSubject::Unconditional);
        let grants = propose_grants(GrantTool::WebSearch, &subjects, Path::new("/work")).unwrap();
        assert_eq!(grants[0].matcher, GrantMatcher::Any);
    }

    #[test]
    fn a_bash_call_proposes_only_commands_the_policy_does_not_approve() {
        let subjects = GrantSubjects::Commands(vec![
            command(&["ls"], true),
            command(&["make", "test"], false),
        ]);
        let grants = propose_grants(GrantTool::Bash, &subjects, Path::new("/work")).unwrap();
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].stored_rule().as_deref(), Some("make test"));
    }

    #[test]
    fn an_unanalysable_or_ungrantable_part_refuses_the_whole_call() {
        let unanalysable = GrantSubjects::Commands(vec![
            command(&["make"], false),
            CommandStatement::Unanalysable,
        ]);
        assert!(propose_grants(GrantTool::Bash, &unanalysable, Path::new("/work")).is_err());
        let interpreter = GrantSubjects::Commands(vec![command(&["python", "-c", "x"], false)]);
        assert!(propose_grants(GrantTool::Bash, &interpreter, Path::new("/work")).is_err());
        assert!(propose_grants(
            GrantTool::Bash,
            &GrantSubjects::NotGrantable,
            Path::new("/work")
        )
        .is_err());
    }

    #[test]
    fn a_fully_policy_approved_call_has_nothing_to_grant() {
        let subjects = GrantSubjects::Commands(vec![command(&["ls"], true)]);
        assert!(propose_grants(GrantTool::Bash, &subjects, Path::new("/work")).is_err());
    }

    #[test]
    fn warnings_name_the_wildcard_marker_and_file_mutation() {
        let subjects =
            GrantSubjects::Commands(vec![command(&["cargo", "test", "a", "b", "c"], false)]);
        let grants = propose_grants(GrantTool::Bash, &subjects, Path::new("/work")).unwrap();
        let warnings = grant_warnings(&grants, Some(Path::new("/work/.ylva")));
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("trailing *")));
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("/work/.ylva")));
        let write = grant_warnings(
            &[Grant {
                tool: GrantTool::Write,
                matcher: GrantMatcher::Any,
            }],
            None,
        );
        assert!(write
            .iter()
            .any(|warning| warning.contains("modifies files")));
    }
}
