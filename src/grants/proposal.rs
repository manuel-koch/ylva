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
    propose_command_rule, CommandRule, CommandRuleScope, CommandStatement, CommandSubject, Grant,
    GrantDraft, GrantError, GrantMatcher, GrantSubject, GrantSubjects, GrantTool, OriginRule,
    PathRule,
};

/// What the grant dialog offers for one pending call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantProposal {
    /// One grant per row of the dialog.
    pub grants: Vec<Grant>,
    /// Simple commands already allowed by the sandbox policy or an existing
    /// grant.
    pub already_allowed: usize,
    /// Parts of the call that can only be approved once (unanalysable
    /// constructs and commands no grant may cover).
    pub other_parts: bool,
}

/// The narrowest reusable grants that would cover `subjects` of a `tool`
/// call: an exact path, skill, origin or one proposed rule per simple command
/// that neither the sandbox policy nor `already_covered` approves.
///
/// Parts of a `bash` call that cannot be granted get no grant and are
/// reported through `other_parts`. Fails when nothing can be granted.
pub fn propose_grants(
    tool: GrantTool,
    subjects: &GrantSubjects,
    workspace_root: &Path,
    already_covered: &dyn Fn(&CommandSubject) -> bool,
) -> Result<GrantProposal, GrantError> {
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
            return propose_command_grants(tool, statements, already_covered);
        }
    };
    Ok(GrantProposal {
        grants: vec![Grant { tool, matcher }],
        already_allowed: 0,
        other_parts: false,
    })
}

/// Validates a stored or edited `draft` against the call it is created for:
/// it must be a valid grant and cover at least one part of the call that the
/// sandbox policy does not already approve. The agent applies this to what
/// the dialog sends, so UI text is never trusted blindly.
pub fn validate_draft(
    draft: &GrantDraft,
    subjects: &GrantSubjects,
    workspace_root: &Path,
) -> Result<Grant, GrantError> {
    let grant = Grant::from_stored(draft.tool, draft.name.as_deref(), draft.rule.as_deref())?;
    let mismatch = || GrantError::new("the rule does not match the call being approved");
    match subjects {
        GrantSubjects::NotGrantable => Err(mismatch()),
        GrantSubjects::Single(subject) => grant
            .covers(subject, workspace_root)
            .then_some(grant)
            .ok_or_else(mismatch),
        GrantSubjects::Commands(statements) => {
            let GrantMatcher::Command(rule) = &grant.matcher else {
                return Err(mismatch());
            };
            let mut explanation = None;
            for statement in statements {
                let CommandStatement::Command {
                    command,
                    policy_approved: false,
                } = statement
                else {
                    continue;
                };
                match CommandRule::parse_for_command(rule.text(), command) {
                    Ok(_) => return Ok(grant),
                    Err(error) => {
                        let same_program = command.tokens.first().map(|token| token.text.as_str())
                            == rule.program();
                        if same_program && explanation.is_none() {
                            explanation = Some(error);
                        }
                    }
                }
            }
            Err(explanation.unwrap_or_else(mismatch))
        }
    }
}

/// A one-line description of `grant` for notices and the grant dialog.
pub fn describe_grant(grant: &Grant) -> String {
    match grant.stored_rule() {
        Some(rule) => format!("{} '{}'", grant.tool.name(), rule),
        None => format!("{} (any call)", grant.tool.name()),
    }
}

/// Consequences of creating `grants` that the user should read before
/// confirming. `persistent` says whether any of them is saved; `marker` is
/// the `.ylva` directory a persistent save would create.
pub fn grant_warnings(grants: &[Grant], persistent: bool, marker: Option<&Path>) -> Vec<String> {
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
    if persistent {
        if let Some(marker) = marker {
            warnings.push(format!(
                "Saving a persistent grant creates {}.",
                marker.display()
            ));
        }
        warnings.push(
            "A saved grant changes the workspace trust fingerprint; run `ylva trust` before \
             the next start."
                .to_string(),
        );
    }
    warnings
}

fn propose_command_grants(
    tool: GrantTool,
    statements: &[CommandStatement],
    already_covered: &dyn Fn(&CommandSubject) -> bool,
) -> Result<GrantProposal, GrantError> {
    let mut proposal = GrantProposal {
        grants: Vec::new(),
        already_allowed: 0,
        other_parts: false,
    };
    for statement in statements {
        let command = match statement {
            CommandStatement::Unanalysable => {
                proposal.other_parts = true;
                continue;
            }
            CommandStatement::Command {
                policy_approved: true,
                ..
            } => {
                proposal.already_allowed += 1;
                continue;
            }
            CommandStatement::Command { command, .. } => command,
        };
        if already_covered(command) {
            proposal.already_allowed += 1;
            continue;
        }
        let Some(rule) = propose_command_rule(command).filter(|rule| rule.matches(command)) else {
            proposal.other_parts = true;
            continue;
        };
        let grant = Grant {
            tool,
            matcher: GrantMatcher::Command(rule),
        };
        if !proposal.grants.contains(&grant) {
            proposal.grants.push(grant);
        }
    }
    if proposal.grants.is_empty() {
        return Err(GrantError::new(if proposal.other_parts {
            "no part of this call can be granted; approve it once"
        } else {
            "this call cannot be granted"
        }));
    }
    Ok(proposal)
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

    fn propose(tool: GrantTool, subjects: &GrantSubjects) -> Result<GrantProposal, GrantError> {
        propose_grants(tool, subjects, Path::new("/work"), &|_| false)
    }

    #[test]
    fn a_path_call_proposes_the_exact_workspace_relative_path() {
        let root = PathBuf::from("/work");
        let subjects = GrantSubjects::Single(GrantSubject::Path(PathBuf::from("/work/src/a.rs")));
        let grants = propose(GrantTool::Write, &subjects).unwrap().grants;
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].stored_rule().as_deref(), Some("./src/a.rs"));
        assert!(grants[0].covers(&GrantSubject::Path(PathBuf::from("/work/src/a.rs")), &root));
    }

    #[test]
    fn an_unconditional_tool_proposes_an_any_grant() {
        let subjects = GrantSubjects::Single(GrantSubject::Unconditional);
        let grants = propose(GrantTool::WebSearch, &subjects).unwrap().grants;
        assert_eq!(grants[0].matcher, GrantMatcher::Any);
    }

    #[test]
    fn a_bash_call_proposes_only_commands_nothing_already_allows() {
        let subjects = GrantSubjects::Commands(vec![
            command(&["ls"], true),
            command(&["git", "status"], false),
            command(&["make", "test"], false),
        ]);
        let proposal = propose_grants(GrantTool::Bash, &subjects, Path::new("/work"), &|command| {
            command.tokens[0].text == "git"
        })
        .unwrap();
        assert_eq!(proposal.grants.len(), 1);
        assert_eq!(
            proposal.grants[0].stored_rule().as_deref(),
            Some("make test")
        );
        assert_eq!(proposal.already_allowed, 2);
        assert!(!proposal.other_parts);
    }

    #[test]
    fn ungrantable_parts_get_no_row_but_are_reported() {
        let subjects = GrantSubjects::Commands(vec![
            command(&["make"], false),
            CommandStatement::Unanalysable,
            command(&["python", "-c", "x"], false),
        ]);
        let proposal = propose(GrantTool::Bash, &subjects).unwrap();
        assert_eq!(proposal.grants.len(), 1);
        assert!(proposal.other_parts);
    }

    #[test]
    fn a_call_with_nothing_grantable_is_refused() {
        let interpreter = GrantSubjects::Commands(vec![
            command(&["python", "-c", "x"], false),
            CommandStatement::Unanalysable,
        ]);
        assert!(propose(GrantTool::Bash, &interpreter).is_err());
        assert!(propose(GrantTool::Bash, &GrantSubjects::NotGrantable).is_err());
        let approved = GrantSubjects::Commands(vec![command(&["ls"], true)]);
        assert!(propose(GrantTool::Bash, &approved).is_err());
    }

    fn draft(tool: GrantTool, rule: &str) -> GrantDraft {
        GrantDraft {
            tool,
            name: None,
            rule: Some(rule.to_string()),
        }
    }

    #[test]
    fn an_edited_command_rule_must_be_valid_and_match_a_command_of_the_call() {
        let subjects = GrantSubjects::Commands(vec![
            command(&["ls"], true),
            command(&["make", "test"], false),
        ]);
        let root = Path::new("/work");
        assert!(validate_draft(&draft(GrantTool::Bash, "make *"), &subjects, root).is_ok());
        let widened = validate_draft(&draft(GrantTool::Bash, "make test *"), &subjects, root);
        assert!(widened.unwrap_err().0.contains("further argument"));
        let other = validate_draft(&draft(GrantTool::Bash, "cargo build"), &subjects, root);
        assert!(other.unwrap_err().0.contains("does not match"));
        let policy_approved = validate_draft(&draft(GrantTool::Bash, "ls"), &subjects, root);
        assert!(policy_approved.is_err());
        let invalid = validate_draft(&draft(GrantTool::Bash, "make **"), &subjects, root);
        assert!(invalid.is_err());
    }

    #[test]
    fn an_edited_path_rule_must_cover_the_target() {
        let subjects = GrantSubjects::Single(GrantSubject::Path(PathBuf::from("/work/src/a.rs")));
        let root = Path::new("/work");
        assert!(validate_draft(&draft(GrantTool::Read, "./src/*"), &subjects, root).is_ok());
        assert!(validate_draft(&draft(GrantTool::Read, "./docs/*"), &subjects, root).is_err());
        assert!(validate_draft(&draft(GrantTool::Read, "**"), &subjects, root).is_err());
    }

    #[test]
    fn warnings_name_the_wildcard_marker_and_file_mutation() {
        let subjects =
            GrantSubjects::Commands(vec![command(&["cargo", "test", "a", "b", "c"], false)]);
        let grants = propose(GrantTool::Bash, &subjects).unwrap().grants;
        let warnings = grant_warnings(&grants, true, Some(Path::new("/work/.ylva")));
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("trailing *")));
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("/work/.ylva")));
        let temporary = grant_warnings(&grants, false, Some(Path::new("/work/.ylva")));
        assert!(!temporary.iter().any(|warning| warning.contains(".ylva")));
        let write = grant_warnings(
            &[Grant {
                tool: GrantTool::Write,
                matcher: GrantMatcher::Any,
            }],
            false,
            None,
        );
        assert!(write
            .iter()
            .any(|warning| warning.contains("modifies files")));
    }
}
