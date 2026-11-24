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

//! Matchers for `bash` grants: one simple command, literal program and
//! literal arguments, with an optional trailing `*`.
//!
//! The trailing `*` means "one or more further arguments" — deliberately
//! different from configured `[tools.bash]` rules, where a trailing `*`
//! stands for exactly one argument and `**` for one or more. Quoting is
//! preserved: a quoted `*` is a literal argument, not the suffix. A grant
//! never covers a command with an unquoted glob, brace expansion or `~`
//! (`git log *` does not approve `git log *.md`).

use super::subject::{CommandSubject, CommandToken};
use super::GrantError;
use crate::sandbox::bash_command_extractor::{
    extract, token_has_unquoted_brace_expansion, token_has_unquoted_glob, ExtractedStatement,
    ExtractionResult,
};

/// Longest leading token run [`propose_command_rule`] copies verbatim.
pub const MAXIMUM_PROPOSED_TOKENS: usize = 4;

/// A validated `bash` grant matcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRule {
    text: String,
    tokens: Vec<CommandToken>,
    trailing_wildcard: bool,
}

/// How widely a command rule matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandRuleScope {
    /// Matches exactly the listed program and arguments.
    Exact,
    /// Matches the listed tokens plus one or more further arguments.
    Prefix,
}

impl CommandRule {
    /// Validate a stored or user-edited rule.
    pub fn parse(text: &str) -> Result<CommandRule, GrantError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(GrantError::new("the command rule must not be empty"));
        }
        if text.contains(['\n', '\r']) {
            return Err(GrantError::new("the command rule must be a single line"));
        }
        let command = match extract(text) {
            ExtractionResult::ParseFailed => {
                return Err(GrantError::new(
                    "the command rule is not valid shell syntax",
                ))
            }
            ExtractionResult::Statements(statements) => match statements.as_slice() {
                [ExtractedStatement::Command(command)] => command.clone(),
                _ => {
                    return Err(GrantError::new(
                        "the command rule must be a single simple command (no pipes, lists, \
                         redirects or substitutions)",
                    ))
                }
            },
        };
        if command.has_variable_assignment_prefix {
            return Err(GrantError::new(
                "the command rule must not start with a variable assignment",
            ));
        }
        if command.backgrounded {
            return Err(GrantError::new("the command rule must not be backgrounded"));
        }
        let mut tokens = Vec::new();
        let mut trailing_wildcard = false;
        let last_index = command.argv.len().saturating_sub(1);
        for (index, token) in command.argv.iter().enumerate() {
            if token_has_unquoted_glob(token) {
                if index == last_index && index >= 1 && !token.quoted && token.text == "*" {
                    trailing_wildcard = true;
                    continue;
                }
                return Err(GrantError::new(
                    "only a single trailing `*` is a wildcard; quote any other `*`, `?` or `[` \
                     to match it literally (`**` is not allowed)",
                ));
            }
            if token_has_unquoted_brace_expansion(token) {
                return Err(GrantError::new(
                    "the command rule must not contain an unquoted brace expansion",
                ));
            }
            if token.unquoted_text.starts_with('~') {
                return Err(GrantError::new("the command rule must not contain `~`"));
            }
            tokens.push(CommandToken::new(token.text.clone(), token.quoted));
        }
        if tokens.is_empty() {
            return Err(GrantError::new("the command rule needs a program"));
        }
        if let Some(reason) = non_grantable_reason(&tokens, trailing_wildcard) {
            return Err(GrantError::new(reason));
        }
        Ok(CommandRule {
            text: text.to_string(),
            tokens,
            trailing_wildcard,
        })
    }

    /// Validate an edited rule for a dialog row: it must keep the row's
    /// program and match the row's actual command.
    pub fn parse_for_command(
        text: &str,
        command: &CommandSubject,
    ) -> Result<CommandRule, GrantError> {
        let rule = CommandRule::parse(text)?;
        let program = command.tokens.first().map(|token| token.text.as_str());
        if program != Some(rule.tokens[0].text.as_str()) {
            return Err(GrantError::new(
                "the program of the rule must stay the program of the command",
            ));
        }
        if !rule.matches(command) {
            return Err(
                if rule.trailing_wildcard && command.tokens.len() <= rule.tokens.len() {
                    GrantError::new(
                        "a trailing `*` needs at least one further argument; widen the rule by \
                     dropping tokens before the `*`",
                    )
                } else {
                    GrantError::new("the rule does not match the command being approved")
                },
            );
        }
        Ok(rule)
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn scope(&self) -> CommandRuleScope {
        if self.trailing_wildcard {
            CommandRuleScope::Prefix
        } else {
            CommandRuleScope::Exact
        }
    }

    /// Whether the rule covers more than the one command it was written for
    /// (wildcard, or an interpreter that runs whatever file it is given).
    pub fn is_wider(&self) -> bool {
        self.trailing_wildcard || is_interpreter(&self.tokens[0].text)
    }

    /// Whether the rule covers `command`.
    pub fn matches(&self, command: &CommandSubject) -> bool {
        if command.has_unquoted_expansion
            || command.has_variable_assignment_prefix
            || non_grantable_reason(&command.tokens, false).is_some()
        {
            return false;
        }
        let argv = &command.tokens;
        let length_matches = if self.trailing_wildcard {
            argv.len() > self.tokens.len()
        } else {
            argv.len() == self.tokens.len()
        };
        length_matches
            && self
                .tokens
                .iter()
                .zip(argv)
                .all(|(expected, actual)| expected.text == actual.text)
    }
}

/// Programs that run whatever file they are given.
const INTERPRETERS: &[&str] = &["python", "node", "perl", "ruby", "php", "sh", "bash", "zsh"];

fn program_name(program: &str) -> &str {
    program.rsplit('/').next().unwrap_or(program)
}

fn is_interpreter(program: &str) -> bool {
    let name = program_name(program);
    INTERPRETERS.iter().any(|interpreter| {
        name.strip_prefix(interpreter).is_some_and(|rest| {
            rest.chars()
                .all(|character| character.is_ascii_digit() || character == '.')
        })
    })
}

/// Inline-code flags: single-letter flags (also inside clusters such as
/// `-lc`) and long flags of an interpreter.
fn inline_code_flags(name: &str) -> (&'static [char], &'static [&'static str]) {
    let base = INTERPRETERS
        .iter()
        .find(|interpreter| {
            name.strip_prefix(**interpreter).is_some_and(|rest| {
                rest.chars()
                    .all(|character| character.is_ascii_digit() || character == '.')
            })
        })
        .copied()
        .unwrap_or("");
    match base {
        "python" => (&['c'], &[]),
        "node" => (&['e', 'p'], &["--eval", "--print"]),
        "perl" | "ruby" => (&['e', 'E'], &[]),
        "php" => (&['r'], &[]),
        "sh" | "bash" | "zsh" => (&['c'], &[]),
        _ => (&[], &[]),
    }
}

fn has_inline_code_flag(name: &str, arguments: &[CommandToken]) -> bool {
    let (short_flags, long_flags) = inline_code_flags(name);
    arguments.iter().any(|argument| {
        let text = argument.text.as_str();
        if let Some(long) = text.strip_prefix("--") {
            long_flags.iter().any(|flag| {
                let flag = &flag[2..];
                long == flag || long.starts_with(&format!("{flag}="))
            })
        } else if let Some(cluster) = text.strip_prefix('-') {
            cluster
                .chars()
                .any(|character| short_flags.contains(&character))
        } else {
            false
        }
    })
}

/// Why no grant may cover a command with these `tokens` (`wildcard`: with a
/// trailing `*`), or `None` when it is grantable. Static table: shell-state
/// builtins, wrappers (never unwrapped), interpreters with inline code,
/// destructive programs with a wildcard, `$`/`~` in matched tokens.
pub fn non_grantable_reason(tokens: &[CommandToken], wildcard: bool) -> Option<&'static str> {
    let program = tokens.first()?.text.as_str();
    let name = program_name(program);
    let arguments = &tokens[1..];
    if matches!(
        name,
        "cd" | "pushd" | "popd" | "export" | "source" | "." | "unset" | "alias" | "eval" | "exec"
    ) {
        return Some("shell-state commands and code evaluation cannot be granted");
    }
    if matches!(
        name,
        "env" | "xargs" | "sudo" | "doas" | "nohup" | "time" | "timeout" | "watch"
    ) {
        return Some("wrapper commands run other programs and cannot be granted");
    }
    if name == "find"
        && (wildcard
            || arguments.iter().any(|argument| {
                matches!(
                    argument.text.as_str(),
                    "-exec" | "-execdir" | "-ok" | "-okdir" | "-delete"
                )
            }))
    {
        return Some("`find` with `-exec`/`-delete` (or a wildcard) cannot be granted");
    }
    if is_interpreter(program) && (wildcard || has_inline_code_flag(name, arguments)) {
        return Some("interpreters with inline code (or a wildcard) cannot be granted");
    }
    if wildcard
        && (matches!(name, "rm" | "dd" | "chmod" | "chown" | "mv") || name.starts_with("mkfs"))
    {
        return Some(
            "destructive programs cannot be granted with a wildcard; grant exact arguments",
        );
    }
    if tokens
        .iter()
        .any(|token| token.text.contains('$') || token.text.starts_with('~'))
    {
        return Some("arguments with `$` or `~` cannot be granted");
    }
    None
}

fn quote_token(token: &CommandToken, is_program: bool) -> String {
    let safe = !token.text.is_empty()
        && !(is_program && token.text.contains('='))
        && token
            .text
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "_@%+=:,./-".contains(character));
    if safe {
        token.text.clone()
    } else {
        format!("'{}'", token.text.replace('\'', "'\\''"))
    }
}

fn is_path_like(text: &str) -> bool {
    text.contains('/') || text.starts_with('.') || text.starts_with('~')
}

/// Propose the default matcher for a dialog row: leading tokens verbatim
/// (flags included), capped at [`MAXIMUM_PROPOSED_TOKENS`]; stops at the
/// first path-like, quoted or glob/expansion token; a trailing `*` only when
/// tokens were dropped, so a short all-literal command proposes an exact
/// matcher. `None` when the proposal is not grantable.
pub fn propose_command_rule(command: &CommandSubject) -> Option<CommandRule> {
    let mut kept = Vec::new();
    for (index, token) in command.tokens.iter().enumerate() {
        let stops = index >= MAXIMUM_PROPOSED_TOKENS
            || (index > 0
                && (token.quoted
                    || is_path_like(&token.text)
                    || token.text.contains(['*', '?', '[', '{', '$', '\\', '`'])));
        if stops {
            break;
        }
        kept.push(quote_token(&command.tokens[index], index == 0));
    }
    let dropped = kept.len() < command.tokens.len();
    let mut text = kept.join(" ");
    if dropped {
        text.push_str(" *");
    }
    let rule = CommandRule::parse(&text).ok()?;
    rule.matches(command).then_some(rule)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(source: &str) -> CommandSubject {
        match extract(source) {
            ExtractionResult::Statements(statements) => match &statements[0] {
                ExtractedStatement::Command(extracted) => CommandSubject {
                    tokens: extracted
                        .argv
                        .iter()
                        .map(|token| CommandToken::new(token.text.clone(), token.quoted))
                        .collect(),
                    has_unquoted_expansion: extracted.argv.iter().any(|token| {
                        token_has_unquoted_glob(token)
                            || token_has_unquoted_brace_expansion(token)
                            || token.unquoted_text.starts_with('~')
                    }),
                    has_variable_assignment_prefix: extracted.has_variable_assignment_prefix,
                },
                other => panic!("not a command: {other:?}"),
            },
            ExtractionResult::ParseFailed => panic!("parse failed"),
        }
    }

    fn covers(rule: &str, source: &str) -> bool {
        CommandRule::parse(rule).unwrap().matches(&command(source))
    }

    #[test]
    fn exact_rules_match_only_the_same_program_and_arguments() {
        assert!(covers("foo", "foo"));
        assert!(!covers("foo", "foo bar"));
        assert!(covers("foo bar", "foo bar"));
        assert!(!covers("foo bar", "foo baz"));
        assert!(!covers("foo bar", "foo"));
    }

    #[test]
    fn trailing_wildcard_needs_one_or_more_further_arguments() {
        assert!(covers("foo bar *", "foo bar -a"));
        assert!(covers("foo bar *", "foo bar -a -b"));
        assert!(!covers("foo bar *", "foo bar"));
        assert!(!covers("foo bar *", "foo baz"));
        assert!(covers("foo -a *", "foo -a file"));
        assert!(!covers("foo -a *", "foo -a"));
    }

    #[test]
    fn argument_boundaries_are_whole_arguments() {
        assert!(!covers("foo bar *", "foo barcode x"));
        assert!(!covers("foo bar", "foo barcode"));
    }

    #[test]
    fn program_is_matched_literally() {
        assert!(!covers("make foo", "/usr/bin/make foo"));
        assert!(!covers("/usr/bin/make foo", "make foo"));
    }

    #[test]
    fn quoted_star_is_a_literal_argument_not_the_suffix() {
        let rule = CommandRule::parse("grep '*'").unwrap();
        assert_eq!(rule.scope(), CommandRuleScope::Exact);
        assert!(rule.matches(&command("grep '*'")));
        assert!(!rule.matches(&command("grep foo")));
        assert_eq!(
            CommandRule::parse("grep *").unwrap().scope(),
            CommandRuleScope::Prefix
        );
    }

    #[test]
    fn unquoted_glob_and_brace_commands_are_never_covered() {
        assert!(!covers("git log *", "git log *.md"));
        assert!(!covers("git log *", "git log {a,b}"));
        assert!(!covers("git log *", "git log ~/x"));
        assert!(covers("git log *", "git log -- '*.md'"));
    }

    #[test]
    fn assignment_prefixed_commands_are_never_covered() {
        assert!(!covers("make test", "CC=gcc make test"));
    }

    #[test]
    fn invalid_rules_are_rejected() {
        for rule in [
            "",
            "*",
            "foo **",
            "foo * bar",
            "foo *.md",
            "foo {a,b}",
            "foo | bar",
            "foo; bar",
            "FOO=1 foo",
            "foo &",
            "foo > out",
            "foo $(bar)",
            "foo ~/x",
            "foo\nbar",
            "foo '",
        ] {
            assert!(
                CommandRule::parse(rule).is_err(),
                "{rule:?} must be rejected"
            );
        }
    }

    #[test]
    fn non_grantable_table_rejects_rules() {
        for rule in [
            "python -c 'print(1)'",
            "python3 *",
            "node -e 'x'",
            "node --eval=x",
            "perl -e x",
            "ruby -e x",
            "php -r x",
            "sh -c x",
            "bash -lc x",
            "zsh -c x",
            "env FOO=1 make",
            "xargs rm",
            "sudo ls",
            "nohup ls",
            "time ls",
            "timeout 5 ls",
            "watch ls",
            "find . -exec rm {} +",
            "find . *",
            "rm -rf *",
            "rm *",
            "dd *",
            "mkfs.ext4 *",
            "chmod 777 *",
            "chown me *",
            "mv a *",
            "cd /tmp",
            "pushd /tmp",
            "export A=b",
            "source env.sh",
            "echo '$HOME'",
        ] {
            assert!(
                CommandRule::parse(rule).is_err(),
                "{rule:?} must be rejected"
            );
        }
    }

    #[test]
    fn destructive_programs_allow_exact_arguments_and_interpreters_allow_files() {
        assert!(CommandRule::parse("rm build.log").is_ok());
        let interpreter_rule = CommandRule::parse("python3 script.py").unwrap();
        assert!(interpreter_rule.is_wider());
        assert!(!CommandRule::parse("git status").unwrap().is_wider());
        assert!(CommandRule::parse("git log *").unwrap().is_wider());
    }

    #[test]
    fn inline_code_subjects_are_not_covered_even_by_a_matching_text() {
        let subject = command("python -c 'print(1)'");
        let rule = CommandRule {
            text: "python -c print(1)".to_string(),
            tokens: subject.tokens.clone(),
            trailing_wildcard: false,
        };
        assert!(!rule.matches(&subject));
    }

    #[test]
    fn proposal_copies_up_to_four_leading_tokens_and_appends_a_suffix() {
        let propose = |source: &str| propose_command_rule(&command(source)).map(|rule| rule.text);
        assert_eq!(propose("git status").as_deref(), Some("git status"));
        assert_eq!(
            propose("cargo test --lib").as_deref(),
            Some("cargo test --lib")
        );
        assert_eq!(
            propose("cargo test --lib grants::tests").as_deref(),
            Some("cargo test --lib grants::tests")
        );
        assert_eq!(
            propose("cargo test --lib --release --quiet name").as_deref(),
            Some("cargo test --lib --release *")
        );
        assert_eq!(
            propose("git log -- src/main.rs").as_deref(),
            Some("git log -- *")
        );
        assert_eq!(
            propose("git commit -m 'msg'").as_deref(),
            Some("git commit -m *")
        );
        assert_eq!(propose("ls ./src").as_deref(), Some("ls *"));
        assert_eq!(propose("make").as_deref(), Some("make"));
    }

    #[test]
    fn proposal_is_none_for_non_grantable_commands() {
        assert_eq!(propose_command_rule(&command("python -c 'x'")), None);
        assert_eq!(propose_command_rule(&command("sudo ls")), None);
        assert_eq!(propose_command_rule(&command("cd /tmp")), None);
        assert_eq!(propose_command_rule(&command("git log *.md")), None);
    }

    #[test]
    fn proposed_rules_roundtrip_through_parse() {
        let subject = command("git log --format 'a b' file");
        let rule = propose_command_rule(&subject).unwrap();
        assert_eq!(rule.text(), "git log --format *");
        assert!(CommandRule::parse(rule.text()).is_ok());
    }

    #[test]
    fn edited_rule_must_keep_the_program_and_match_the_command() {
        let subject = command("make foo");
        assert!(CommandRule::parse_for_command("make *", &subject).is_ok());
        assert!(CommandRule::parse_for_command("make foo", &subject).is_ok());
        assert!(CommandRule::parse_for_command("make bar", &subject).is_err());
        assert!(CommandRule::parse_for_command("*", &subject).is_err());
        assert!(CommandRule::parse_for_command("/usr/bin/make foo", &subject).is_err());
        let error = CommandRule::parse_for_command("make foo *", &subject).unwrap_err();
        assert!(error.0.contains("further argument"), "{error}");
        assert!(CommandRule::parse_for_command("make foo\n", &subject).is_ok());
        assert!(CommandRule::parse_for_command("make foo\nrm x", &subject).is_err());
    }

    #[test]
    fn edited_rule_cannot_reintroduce_a_non_grantable_wildcard() {
        let subject = command("rm build.log");
        assert!(CommandRule::parse_for_command("rm *", &subject).is_err());
        assert!(CommandRule::parse_for_command("rm build.log", &subject).is_ok());
    }
}
